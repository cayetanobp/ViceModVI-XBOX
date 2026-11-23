//! Runs the `vicemodvi` binary against synthetic sources.

#[path = "../../vicemodvi-core/tests/support/mod.rs"]
mod support;

use std::{path::Path, process::Command};

use serde_json::Value;

fn run(args: &[&str]) -> (i32, Value, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_vicemodvi")).args(args).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let json = serde_json::from_str(&stdout).unwrap_or(Value::Null);
    (output.status.code().unwrap_or(-1), json, stderr)
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn usage_and_errors_use_exit_codes() {
    let (code, _, stderr) = run(&[]);
    assert_eq!(code, 2);
    assert!(stderr.contains("write-rpf-copy"));
    let (code, _, stderr) = run(&["verify", "missing-catalog.json"]);
    assert_eq!(code, 1);
    assert!(!stderr.is_empty());
}

#[test]
fn catalog_list_export_and_pool_copy() {
    let dir = tempfile::tempdir().unwrap();
    let sources = dir.path().join("sources");
    std::fs::create_dir(&sources).unwrap();
    std::fs::write(sources.join("pool.ttpf"), support::pool(&[(256, 256, 71, 9, 0x1111), (64, 64, 28, 7, 0x2222)])).unwrap();
    std::fs::write(sources.join("archive.rpf"), support::archive()).unwrap();
    let catalog = dir.path().join("catalog.json");

    let (code, counts, stderr) = run(&["catalog", text(&catalog), "--fresh", text(&sources)]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(counts, serde_json::json!({ "dictionary": 1, "texture": 4 }));

    let (code, listed, _) = run(&["list", text(&catalog), "--type", "texture", "--hash", "0x2222"]);
    assert_eq!(code, 0);
    let listed = listed.as_array().unwrap();
    assert_eq!(listed.len(), 1);
    let pool_texture = listed[0]["id"].as_str().unwrap().to_string();
    assert_eq!(listed[0]["nativeKey"], "0000000000002222");

    let (code, verified, _) = run(&["verify", text(&catalog)]);
    assert_eq!(code, 0);
    assert_eq!(verified.as_array().unwrap().len(), 2);

    let dds = dir.path().join("t.dds");
    let (code, export, stderr) = run(&["export", text(&catalog), &pool_texture, text(&dds)]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(export["coverage"], "complete mip chain");
    let (code, inspected, _) = run(&["inspect", text(&dds)]);
    assert_eq!(code, 0);
    assert_eq!(inspected["dxgiFormat"], 28);
    let (code, _, stderr) = run(&["export", text(&catalog), &pool_texture, text(&dds)]);
    assert_eq!(code, 1, "exports never overwrite: {stderr}");

    let source_id = listed[0]["sourceId"].as_str().unwrap().to_string();
    let (code, profiles, _) = run(&["pool-profiles", text(&catalog), &source_id]);
    assert_eq!(code, 0);
    assert_eq!(profiles[1]["supportsNativeReplacement"], true);

    let copy = dir.path().join("copy.ttpf");
    let (code, result, stderr) = run(&["write-pool-copy", text(&catalog), &pool_texture, text(&dds), text(&copy)]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(result["contentChanged"], false);
    assert_eq!(result["conversion"]["requiresConversion"], false);
    let (code, _, stderr) = run(&["write-pool-copy", text(&catalog), &pool_texture, text(&dds), text(&copy), "--bogus"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("--overwrite"));

    let converted = dir.path().join("converted.dds");
    let png = dir.path().join("in.png");
    image::RgbaImage::from_pixel(32, 32, image::Rgba([1, 2, 3, 255])).save(&png).unwrap();
    let (code, plan, stderr) = run(&["convert-for-ttpf", text(&catalog), &pool_texture, text(&png), text(&converted), "--resize"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(plan["output"]["width"], 64);
    assert!(converted.exists());
}
