//! ViceModVI command line. Commands, arguments and JSON output match the C#
//! build. Exit codes: 0 success, 1 handled error, 2 usage.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
};

use serde::Serialize;
use serde_json::{json, Map, Value};
use vicemodvi_core::{
    catalog::Catalog,
    dds, files, manifests, obj_reader, pool_writer, rpf_copy,
    texture_conversion,
    texture_links::{self, TextureIndex},
    util::{eq_ignore_case, full_path, parse_number, path_text},
    xbox_assets::{self, BuildOptions},
    Cancel, Error, Result,
};

const USAGE: &str = "inventory-tsv LEONIDA_INDEX_JSON INVENTORY_TSV
catalog CATALOG [--fresh] [--inventory INVENTORY_TSV] RPF_OR_TTPF_OR_FOLDER ...
import-manifest CATALOG SOURCE_ID MANIFEST_JSON
list CATALOG [--source TEXT] [--type TYPE] [--name TEXT] [--hash HEX] [--offset OFFSET]
read CATALOG ASSET_ID OUTPUT [ALTERNATE_SOURCE]
verify CATALOG
export CATALOG ASSET_ID OUTPUT
inspect DDS_OR_OBJ
resolve CATALOG [MODEL_ID]
convert-for-ttpf CATALOG TEXTURE_ID IMAGE OUTPUT_DDS [--resize]
write-pool-copy CATALOG TEXTURE_ID IMAGE OUTPUT_TTPF [--resize] [--overwrite]
write-rpf-copy CATALOG TEXTURE_ID IMAGE OUTPUT_RPF [--resize]
pool-profiles CATALOG SOURCE_ID";

enum Outcome {
    Done,
    Usage,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match run(&args) {
        Ok(Outcome::Done) => ExitCode::SUCCESS,
        Ok(Outcome::Usage) => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

fn print(value: &impl Serialize) -> Result<()> {
    let text = files::to_json(value)?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{text}")?;
    Ok(())
}

fn path(value: &str) -> &Path {
    Path::new(value)
}

fn run(args: &[&str]) -> Result<Outcome> {
    let none = Cancel::none();
    match args {
        ["inventory-tsv", index, inventory] => xbox_assets::convert_carve_index(path(index), path(inventory))?,
        ["catalog", output, inputs @ ..] if !inputs.is_empty() => {
            let mut inputs = inputs;
            let use_cache = inputs[0] != "--fresh";
            if !use_cache {
                inputs = &inputs[1..];
            }
            let mut inventory = None;
            if let ["--inventory", file, rest @ ..] = inputs {
                inventory = Some(PathBuf::from(file));
                inputs = rest;
            }
            let mut files = Vec::new();
            for input in inputs {
                let input = path(input);
                if input.is_dir() {
                    files.extend(xbox_assets::source_files(input)?);
                } else {
                    files.push(input.to_path_buf());
                }
            }
            let options = BuildOptions { inventory: inventory.as_deref(), use_cache, progress: None };
            let mut catalog = xbox_assets::build(&files, options, &none)?;
            catalog.save(path(output), false)?;
            let mut counts = Map::new();
            for asset in &catalog.assets {
                let entry = counts.entry(asset.kind.clone()).or_insert(json!(0));
                *entry = json!(entry.as_i64().unwrap_or(0) + 1);
            }
            print(&Value::Object(counts))?;
        }
        ["import-manifest", file, source_id, manifest] => {
            let mut catalog = Catalog::load(path(file))?;
            let result = manifests::import(&mut catalog, source_id, path(manifest))?;
            catalog.save(path(file), true)?;
            print(&result)?;
        }
        ["list", file, filters @ ..] => {
            if filters.len() % 2 != 0 {
                return Err(Error::argument("Filters require a value."));
            }
            let mut options: Vec<(&str, &str)> = Vec::new();
            for pair in filters.chunks(2) {
                if !matches!(pair[0], "--source" | "--type" | "--name" | "--hash" | "--offset") {
                    return Err(Error::argument(format!("Unknown filter {}", pair[0])));
                }
                if options.iter().any(|(name, _)| *name == pair[0]) {
                    return Err(Error::argument(format!("An item with the same key has already been added. Key: {}", pair[0])));
                }
                options.push((pair[0], pair[1]));
            }
            let get = |name: &str| options.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);
            let catalog = Catalog::load(path(file))?;
            let offset = get("--offset").map(parse_number).transpose()?;
            print(&catalog.find(get("--source"), get("--type"), get("--name"), get("--hash"), offset)?)?;
        }
        ["read", file, id, output, extra @ ..] if extra.len() <= 1 => {
            let catalog = Catalog::load(path(file))?;
            let asset = catalog.get(id)?;
            let alternate = extra.first().map(|p| path(p));
            let mut measure = None;
            files::write(
                path(output),
                |stream| {
                    measure = Some(catalog.copy_stored(asset, stream, alternate)?);
                    Ok(())
                },
                false,
            )?;
            print(&measure)?;
        }
        ["verify", file] => {
            let catalog = Catalog::load(path(file))?;
            let mut results = Vec::new();
            for source in &catalog.sources {
                let mut stream = source.open()?;
                let bytes = source.fingerprint.check(source.path(), &mut stream, true)?;
                results.push(json!({ "id": source.id, "sha256": source.fingerprint.sha256, "bytes": bytes }));
            }
            print(&results)?;
        }
        ["export", file, id, output] => {
            let catalog = Catalog::load(path(file))?;
            print(&xbox_assets::export(&catalog, catalog.get(id)?, path(output))?)?;
        }
        ["resolve", file, ids @ ..] if ids.len() <= 1 => {
            let catalog = Catalog::load(path(file))?;
            let models = match ids {
                [id] => vec![catalog.get(id)?],
                _ => catalog.find(None, Some("model"), None, None, None)?,
            };
            let index = TextureIndex::new(&catalog);
            let mut rows = Vec::new();
            for asset in models {
                let model = xbox_assets::read_model(&catalog, asset)?;
                for link in texture_links::resolve_model(&catalog, &index, asset, &model)? {
                    rows.push((asset.id.clone(), link));
                }
            }
            let mut counts = Map::new();
            for status in ["confirmed", "ambiguous", "unresolved"] {
                counts.insert(status.into(), json!(rows.iter().filter(|(_, link)| link.status == status).count()));
            }
            let links: Vec<Value> = rows.iter().map(|(model, link)| json!({ "model": model, "link": link })).collect();
            print(&json!({ "counts": counts, "links": links }))?;
        }
        ["write-pool-copy", file, id, input, output, options @ ..] => {
            let distinct = options.iter().enumerate().all(|(i, o)| !options[..i].contains(o));
            if options.iter().any(|o| !matches!(*o, "--overwrite" | "--resize")) || !distinct {
                return Err(Error::argument("Supported pool copy flags are --overwrite and --resize."));
            }
            if eq_ignore_case(&path_text(&full_path(output)?), &path_text(&full_path(file)?)) {
                return Err(Error::io("Choose a destination separate from the catalog."));
            }
            let mut catalog = Catalog::load(path(file))?;
            let asset = catalog.get(id)?.clone();
            let dds_bytes = texture_conversion::import(path(input))?;
            let result = pool_writer::copy(
                &mut catalog,
                &asset,
                &dds_bytes,
                path(output),
                options.contains(&"--overwrite"),
                options.contains(&"--resize"),
                &none,
            )?;
            if result.replaced_source {
                catalog.save(path(file), true)?;
            }
            print(&result)?;
        }
        ["convert-for-ttpf", file, id, input, output, options @ ..] if matches!(options, [] | ["--resize"]) => {
            let catalog = Catalog::load(path(file))?;
            let dds_bytes = texture_conversion::import(path(input))?;
            let plan = pool_writer::plan(&catalog, catalog.get(id)?, &dds_bytes, &none)?;
            let prepared = texture_conversion::apply(&dds_bytes, &plan, options.len() == 1)?;
            files::write_bytes(path(output), &prepared, false)?;
            print(&plan)?;
        }
        ["write-rpf-copy", file, id, input, output, options @ ..] if matches!(options, [] | ["--resize"]) => {
            let catalog = Catalog::load(path(file))?;
            let dds_bytes = texture_conversion::import(path(input))?;
            print(&rpf_copy::copy(&catalog, catalog.get(id)?, &dds_bytes, path(output), options.len() == 1, &none)?)?;
        }
        ["pool-profiles", file, source_id] => {
            let catalog = Catalog::load(path(file))?;
            let source = catalog
                .sources
                .iter()
                .find(|s| s.id == *source_id && s.format == "ttpf")
                .ok_or_else(|| Error::invalid(format!("No TTPF source {source_id} in the catalog.")))?;
            let mut stream = source.open()?;
            source.fingerprint.check(source.path(), &mut stream, false)?;
            let pool = vicemodvi_core::pool::inspect(source.path(), &none)?;
            let rows: Vec<Value> = pool
                .textures
                .iter()
                .map(|t| {
                    json!({
                        "assetId": format!("{}:ttpf:{}", source.id, t.index),
                        "index": t.index,
                        "name": t.name,
                        "dxgiFormat": t.dxgi_format,
                        "width": t.width,
                        "height": t.height,
                        "mips": t.mips,
                        "sourceMips": t.source_mips,
                        "supportsNativeReplacement": t.supports_native_replacement,
                        "limitation": t.limitation,
                    })
                })
                .collect();
            print(&rows)?;
        }
        ["inspect", file] if vicemodvi_core::util::extension(path(file)) == ".dds" => {
            let info = dds::inspect(&std::fs::read(file)?)?;
            print(&json!({
                "width": info.width,
                "height": info.height,
                "mips": info.mips,
                "dxgiFormat": info.dxgi_format,
                "sliceCount": info.slice_count,
                "dataSize": info.data_size,
            }))?;
        }
        ["inspect", file] if vicemodvi_core::util::extension(path(file)) == ".obj" => {
            let text = std::fs::read_to_string(file)?;
            let model = obj_reader::parse(&text, "Imported OBJ")?;
            print(&json!({
                "vertices": model.meshes.iter().map(|m| m.positions.len()).sum::<usize>(),
                "faces": model.meshes.iter().map(|m| m.indices.len()).sum::<usize>() / 3,
                "meshes": model.meshes.len(),
            }))?;
        }
        _ => return Ok(Outcome::Usage),
    }
    Ok(Outcome::Done)
}
