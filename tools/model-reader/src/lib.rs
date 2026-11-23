use rpf9_identify::{identify_with_progress, resource::{classify, Kind}};
use rpf9_index::{extract, Entry, Evidence, Payload};
use rpf9_models::{decode_geometry, drawables};
use std::{fs::File, io::{self, BufReader, Write}};

type Input = BufReader<File>;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn image(input: &mut Input, entries: &[Entry]) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for entry in entries {
        if entry.decoded_len > 256 * 1024 * 1024 - bytes.len() {
            return Err(invalid("Resource image exceeds its limit"));
        }
        bytes.extend(extract(input, entry, Payload::Decoded)?);
    }
    Ok(bytes)
}

fn locator(entries: &[Entry]) -> String {
    entries.iter().map(|e| format!("{},{},{},{},{}", e.offset, e.stored_end,
        e.decoded_len, e.chunks, u8::from(e.evidence == Evidence::ZlibSequence)))
        .collect::<Vec<_>>().join(";")
}

fn parse_part(value: &str) -> io::Result<Entry> {
    let fields = value.split(',').map(str::parse::<u64>).collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid("Invalid model range"))?;
    let &[offset, end, size, chunks, codec] = fields.as_slice() else {
        return Err(invalid("Malformed model range"));
    };
    if offset >= end || codec > 1 || size > 256 * 1024 * 1024 {
        return Err(invalid("Invalid model range"));
    }
    Ok(Entry { ordinal: 0, header: 0, offset, stored_end: end, decoded_len: size as usize,
        chunks: chunks as usize, signature: "unknown",
        evidence: if codec == 1 { Evidence::ZlibSequence } else { Evidence::ZlibStream } })
}

fn list(input: &mut Input, out: &mut impl Write, progress: Progress) -> io::Result<()> {
    let report = identify_with_progress(input, |phase, done, total, unit| {
        report_progress(progress, phase, done, total, unit);
    })?;
    writeln!(out, "# VMI1")?;
    for entry in &report.inventory.entries {
        if matches!(entry.evidence, Evidence::ZlibSequence | Evidence::ZlibStream | Evidence::ZlibUnterminated) {
            writeln!(out, "offset\t{}", entry.offset)?;
        }
    }
    let total = report.resources.len();
    for (done, resource) in report.resources.iter().enumerate() {
        if done % 32 == 0 { report_progress(progress, "Reading resources", done as u64, total as u64, "entries"); }
        if !matches!(&resource.kind, Kind::NamedDictionary { extension, .. } |
            Kind::Named { extension, .. } if extension == "#dr") { continue; }
        let entry = |i: &usize| report.inventory.entries[*i].clone();
        let virt: Vec<_> = std::iter::once(&resource.root_ordinal)
            .chain(&resource.virtual_parts).map(entry).collect();
        let phys: Vec<_> = resource.physical.iter().map(entry).collect();
        let Some(last) = phys.last() else { continue; };
        if virt.iter().chain(&phys).any(|p| !matches!(p.evidence,
            Evidence::ZlibSequence | Evidence::ZlibStream)) { continue; }
        let data = image(input, &virt)?;
        let Ok(items) = drawables(&data, &resource.kind, resource.physical_len as usize) else { continue; };
        let (v, p) = (locator(&virt), locator(&phys));
        for (i, item) in items.iter().enumerate() {
            if !item.lods.iter().flatten().any(|model| !model.geometries.is_empty()) { continue; }
            let name = item.name.clone().unwrap_or_else(|| format!("drawable_{}_{i}", resource.id));
            writeln!(out, "{}\t{}\t{i}\t{name}\t{i}|{v}|{p}", virt[0].offset, last.stored_end)?;
        }
    }
    report_progress(progress, "Reading resources", total as u64, total as u64, "entries");
    Ok(())
}

fn mesh(input: &mut Input, value: &str, out: &mut impl Write) -> io::Result<()> {
    let fields: Vec<_> = value.split('|').collect();
    let [index, virt, phys] = fields.as_slice() else { return Err(invalid("Malformed drawable locator")); };
    let index: usize = index.parse().map_err(|_| invalid("Invalid drawable index"))?;
    let decode = |input: &mut Input, parts: &str| {
        let entries = parts.split(';').map(parse_part).collect::<io::Result<Vec<_>>>()?;
        image(input, &entries)
    };
    let virt = decode(input, virt)?;
    let phys = decode(input, phys)?;
    let items = drawables(&virt, &classify(&virt), phys.len()).map_err(invalid)?;
    let item = items.get(index).ok_or_else(|| invalid("Drawable index is out of range"))?;
    let mut meshes = Vec::new();
    for lod in &item.lods {
        for model in lod {
            for i in 0..model.geometries.len() {
                meshes.push(decode_geometry(&phys, model, i).map_err(invalid)?);
            }
        }
        if !meshes.is_empty() { break; }
    }
    out.write_all(b"VMM2")?;
    out.write_all(&(item.at as u32).to_le_bytes())?;
    out.write_all(&(virt.len() as u32).to_le_bytes())?;
    out.write_all(&virt)?;
    out.write_all(&(meshes.len() as u32).to_le_bytes())?;
    for m in meshes {
        for field in [m.positions.len() as u32, m.triangles.len() as u32,
            u32::from(m.uv0.is_some()), u32::from(m.shader)] {
            out.write_all(&field.to_le_bytes())?;
        }
        for p in m.positions.into_iter().flatten() { out.write_all(&p.to_le_bytes())?; }
        for i in m.triangles.into_iter().flatten() { out.write_all(&i.to_le_bytes())?; }
        if let Some(uv) = m.uv0 {
            for x in uv.into_iter().flatten() { out.write_all(&x.to_le_bytes())?; }
        }
    }
    Ok(())
}


struct Cancelled;

type Output = extern "C" fn(*const u8, usize) -> i32;
type Progress = extern "C" fn(*const u8, usize, u64, u64, *const u8, usize) -> i32;

fn report_progress(callback: Progress, phase: &str, done: u64, total: u64, unit: &str) {
    if callback(phase.as_ptr(), phase.len(), done, total, unit.as_ptr(), unit.len()) == 0 {
        std::panic::resume_unwind(Box::new(Cancelled));
    }
}

/// # Safety
/// path must point to a valid UTF-8 C string. locator must be null for an
/// inventory, or point to a valid UTF-8 C string for a mesh. Callbacks may
/// read borrowed buffers only for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn vicemodvi_read_model(
    path: *const std::ffi::c_char,
    locator: *const std::ffi::c_char,
    output: Output,
    error: Output,
    progress: Progress,
) -> i32 {
    let result = std::panic::catch_unwind(|| -> io::Result<()> {
        let path = std::ffi::CStr::from_ptr(path).to_str().map_err(|e| invalid(e.to_string()))?;
        let mut input = BufReader::new(File::open(path)?);
        let mut bytes = Vec::new();
        if locator.is_null() {
            list(&mut input, &mut bytes, progress)?;
        } else {
            let locator = std::ffi::CStr::from_ptr(locator).to_str().map_err(|e| invalid(e.to_string()))?;
            mesh(&mut input, locator, &mut bytes)?;
        }
        if output(bytes.as_ptr(), bytes.len()) == 0 {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "Model output was rejected"));
        }
        Ok(())
    });
    let message = match result {
        Ok(Ok(())) => return 0,
        Ok(Err(error)) => error.to_string(),
        Err(panic) if panic.is::<Cancelled>() => return 2,
        Err(_) => "The native model reader failed unexpectedly.".to_string(),
    };
    error(message.as_ptr(), message.len());
    1
}
