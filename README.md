<p align="center">
  <img src="assets/leonida-logo-transparent.png" alt="Leonida logo" width="200">
</p>

<h1 align="center">ViceModVI - Xbox</h1>

<p align="center">
  <strong>Browse Xbox Series assets on Windows and preview texture edits on the model.</strong>
</p>

<p align="center">
  <a href="#project-status">Status</a> ·
  <a href="#how-it-works">How it works</a> ·
  <a href="#build">Build</a> ·
  <a href="#local-replacement-workflow">Walkthrough</a> ·
  <a href="CONTRIBUTING.md">Contribute</a>
</p>

ViceModVI is a Windows desktop viewer and CLI for local **Grand Theft Auto VI**
assets. Open an **RPF9 archive** or **TTPF texture pool**, follow a material to
its texture, and export the asset or try a replacement in the desktop preview.

> [!IMPORTANT]
> Bring your own readable Xbox Series files. The repository provides the tool's
> source code. Keep game files, generated catalogs, and extracted assets local.

## Project status

The documented test sample comes from **Xbox Series build 1.0.2.1**. The
archive writers reopen their output and verify it offline. Loading those copies
in the game and restoring the original are pending.

| Capability | Status | Coverage |
| --- | --- | --- |
| Browse and search | **● Available** | Open a single archive or a folder. Search by name, asset ID, type, source path, hash, or offset. [Catalog evidence](PENDING.md#mf-01--catalog-and-byte-offset-reads) |
| Preview and export | **● Available** | Preview textures and static meshes. Export DDS or OBJ, save a single texture as PNG, or export a batch. [Viewer evidence](PENDING.md#mf-07--browse-preview-export-and-edit-in-one-place) |
| Texture replacements | **● Available** | Compare a staged image on the mesh, export the staged DDS, or revert the edit. [Desktop walkthrough](#local-replacement-workflow) |
| TTPF copies | **● Available** | Replace one texture in a compatible RDS5 pool through the CLI. [TTPF checks](#experimental-ttpf-copy) |
| RPF9 copies | **● Available** | Replace one 2D texture in a recovered dictionary when the changed segments retain their packed lengths. [RPF9 checks](#experimental-rpf9-copy) |
| Installation-wide index | **○ Pending** | Persist a game-wide index and reuse it when browsing a subfolder. [Roadmap](PENDING.md#mf-08--installation-root-and-incremental-global-index) |

See [pending work](PENDING.md) for acceptance criteria and planned index packs.
PS5 and PC game assets still need their own format profiles and platform tests.

## How it works

![ViceModVI workbench showing a 3D model and the asset inspector](assets/vicemodvi-workbench.png)

| Step | What ViceModVI does |
| --- | --- |
| **Open** | Use **Open file** for one `.rpf` or `.ttpf`, or **Open folder** to catalog both types together. The viewer loads vicemodvi-catalog.json from the root when present. If the installation folder was moved or renamed, the viewer resolves the catalog paths under the selected folder and checks the source fingerprints. The viewer reuses cached source indexes when the file size, timestamp, and sampled bytes still match. |
| **Trace** | Select a mesh and material. When the viewer confirms a diffuse texture by its native key, **View linked texture** jumps to that entry, including in another open archive or pool. The catalog keeps source locators and distinguishes recovered names from aliases. |
| **Preview and export** | Inspect the texture and its available mips or preview a static model with its resolved diffuse image. The inspector can select a texture candidate and export OBJ, DDS, or a transparent PNG of the selected mesh UVs. Right-click for more export options. DDS retains the available format and mips, PNG saves the decoded image, and OBJ includes geometry and available UVs. |
| **Edit a texture** | Use **Import replacement…** to import an image and toggle **Original** and **Replacement**. Select **Confirm** to save the texture for future previews, or **Revert replacement** to discard the pending edit. Saved replacements live in `.vicemodvi-textures` beside the source archives. Use the CLI when you need a verified archive copy. |

### Data limitations

Inventory TSVs add stored byte ranges to the catalog. A compressed range can be
just one segment of an asset. The original RPF9 path table is still unresolved,
so recovered entries may appear under aliases or offsets. TTPF entries keep
their native keys when readable names are unavailable. The model preview covers
static meshes and confirmed diffuse textures. Native shader effects, blending,
and animations require separate support.

## Build

Use **Windows**, **Git**, and stable **Rust with the MSVC build tools**. One
`cargo build` produces the CLI and the desktop viewer as standalone
executables. No .NET runtime or separate DLL is required. For the sample CLI
walkthrough, you also need Python 3 and Leonida's `rpf9_carve.py`, or a
compatible inventory TSV.

Run from the ViceModVI-XBOX repository root. The readers and writers for the
native formats (DDS, TTPF/RDS5 pools, RPF9 texture dictionaries and drawables,
OBJ, and resource recovery) are ViceModVI's own code in
`crates/vicemodvi-core/src/native`.

```powershell
cargo build --release
$cli = (Resolve-Path target/release/vicemodvi.exe).Path
$viewer = (Resolve-Path target/release/vicemodvi-desktop.exe).Path
& $viewer
```

`cargo test --workspace` runs the unit tests and end-to-end checks on synthetic
RPF9 and TTPF files. To try the viewer without game files, write the same
synthetic sources to a folder and open it:

```powershell
cargo run -p vicemodvi-core --example synthetic -- .local/synthetic
& $viewer (Resolve-Path .local/synthetic).Path
```

In the viewer, choose **Open file** or **Open folder**. Select a node in the
source tree to narrow the list and search results. The status bar reports scan
progress. Closing the viewer during indexing keeps completed source indexes.
To measure an uncached CLI scan, use `catalog OUTPUT --fresh INPUT`.

Folders are indexed in parallel, one source per worker, and the model and
texture candidates of each RPF are validated in parallel batches. Catalog
JSON and `.vmp` files written by the C# build still load.

### Model reader

The CLI and the viewer link the in-repo Rust model reader directly. The reader
inventories RPFs, lists individual drawables, and decodes supported geometry on
selection or export. Supported materials can resolve diffuse textures by full
native key across the loaded TTPF pools.

The vendored crates are from `gta6-resource-tools` revision
`3e6f69cb840112dc7b198f2e23a14520e493dc23` with the indexing and progress patch
already applied. The build includes only the RPF9 reader modules it uses. The upstream license and notices are retained.

## Reproduce the sample

Use a local copy of the **Xbox Series 1.0.2.1** sample with
`core_restofworld.rpf` and its `data/ttdb` pools. The commands assume it is at
`core_restofworld/core_restofworld`. Change the `$sample` path if needed.

The expected SHA-256 of `core_restofworld.rpf` is
`a28e53eee58e0b7541bee699ca0136d7a8402934984fe1d12d573905a24da5f0`.

### Local replacement workflow

1. In the viewer, use **Open folder** to open the sample. Select
   `core_restofworld.rpf` in the tree, search for `0:model:203F010`, and select
   the mesh using shader 1.
2. Choose **View linked texture** to reach entry 90 in
   `data/ttdb/props/ttdb_props.ttpf`. Use **Export selection** from the context
   menu to save the texture as DDS or PNG. Ctrl-select multiple entries to
   export them into one folder.
3. Choose **Import replacement…** in the inspector. Toggle
   **Original** and **Replacement** to compare the image in the texture
   preview or on the mesh. Choose **Confirm** to use it the next time you open the model. **Export staged DDS** saves a separate copy as
   a file, or **Revert replacement** to clear it.

Supported DDS files are staged byte for byte. Raster imports become a one-mip
8-bit BGRA DDS: PNG and APNG, JPEG, BMP, GIF, TIFF, TGA, WebP, ICO, QOI,
OpenEXR, Radiance HDR, the PNM family, and PSD. For animated, paged, or layered
images, the reader uses the first image or the PSD composite. Some legacy DDS
encodings can also use the raster decoder as a fallback.

<details>
<summary>CLI commands for catalog, export, and reference checks</summary>

Run from the repository root after building. If you already have a compatible
`rpf9-index` TSV, set `$index` to it and skip the Python and `inventory-tsv`
commands.

```powershell
$sample = (Resolve-Path core_restofworld/core_restofworld).Path
$out = Join-Path $env:TEMP ("vicemodvi-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $out | Out-Null
$rpf = Join-Path $sample 'core_restofworld.rpf'
$index = Join-Path $out 'index.tsv'
$catalog = Join-Path $out 'catalog.json'

python -I ../Leonida/tools/rpf9_carve.py $rpf $out --index-only
& $cli inventory-tsv (Join-Path $out 'index.json') $index
& $cli catalog $catalog --inventory $index $sample
& $cli verify $catalog
& $cli read $catalog '0:range:4010' (Join-Path $out 'range.bin')

$mesh = Join-Path $out 'reference.obj'
$dds = Join-Path $out 'reference.dds'
& $cli export $catalog '0:model:203F010' $mesh
& $cli export $catalog '3:ttpf:0' $dds
& $cli inspect $mesh
& $cli inspect $dds
& $cli resolve $catalog '0:model:203F010'
& $cli resolve $catalog
```

**Read validation.** Ordinary reads check file length, modification time, and
up to eight 4 KiB samples. `verify` and raw reads with an explicit alternate
source check the full SHA-256. The following read should fail because the
alternate source is a different container.

```powershell
$wrongSource = Join-Path $sample 'data/ttdb/textures_loose.ttpf'
& $cli read $catalog '0:range:4010' (Join-Path $out 'wrong-source.bin') $wrongSource
```

For a bounds check, copy the catalog JSON, set `range.offset` of
`0:range:4010` to the RPF's byte length, and retry the ordinary raw read using
the edited catalog and a fresh output path.

**Manifest and ambiguity checks.** Import Leonida `rpf9dump` manifests with
`& $cli import-manifest $catalog SOURCE_ID MANIFEST_JSON`. RPF manifests use
`Items` with `Offset`. TTPF manifests use `Textures` with `Index` and `AssetKey`.
Native types still come from structure validation. To demonstrate ambiguity,
copy `data/ttdb/props/ttdb_props.ttpf` outside the sample, append that copy to
the inputs after `$sample` when making a second catalog, and resolve the same
model against it.

</details>

### Experimental TTPF copy

Use `pool-profiles` to find entries with writable storage. In the viewer,
**Convert for TTPF export** shows input and output dimensions, DXGI format,
and mip counts. It exports a compatible DDS and keeps the staged image
available in the preview. For a TTPF copy, use the CLI.

<details>
<summary>Conversion and offline verification commands</summary>

Use `$cli`, `$catalog`, and `$out` from the build and sample commands.
Entry 4 in source 3 is writable. Entry 0 exposes three of its eight mips
and demonstrates a storage limitation.

```powershell
& $cli pool-profiles $catalog 3

$full = Join-Path $out 'writable.dds'
$edit = Join-Path $out 'edited.dds'
& $cli export $catalog '3:ttpf:4' $full
$bytes = [IO.File]::ReadAllBytes($full)
$bytes[$bytes.Length - 1] = $bytes[$bytes.Length - 1] -bxor 1
[IO.File]::WriteAllBytes($edit, $bytes)
& $cli write-pool-copy $catalog '3:ttpf:4' $edit (Join-Path $out 'edited.ttpf')
```

Pass a PNG, TGA, or incompatible DDS to `convert-for-ttpf` for a standalone
DDS, or to `write-pool-copy` to produce a TTPF. Compatible DDS inputs pass
through byte for byte. For a matching format that only needs more mip levels,
conversion preserves the supplied mip bytes and generates the rest. Encoding
supports common uncompressed DXGI layouts and BC1, BC2, BC3, BC4 UNORM,
BC5 UNORM, and BC7. Compatible signed BC and BC6H DDS files can pass through.
Resizing requires confirmation in the viewer or `--resize` in the CLI.

The writer reopens the pool and checks the replacement, a neighbor, and the
original fingerprint before publishing. Use a fresh destination for the copy.
`--overwrite` allows an existing destination. If that destination is the source,
the CLI refreshes its catalog fingerprint.

</details>

### Experimental RPF9 copy

The CLI can replace a 2D texture in a recovered standalone dictionary when
each changed zlib segment fits its original packed length. It reopens the copy,
checks the selected DDS and a neighbor when present, and verifies that every
byte outside the replaced segments is identical.

<details>
<summary>Write and reopen a fixed-size dictionary copy</summary>

Use `$cli`, `$catalog`, `$index`, and `$out` from the build and sample commands.

```powershell
$target = '0:dictionary:680010:texture:0'
$untouched = Join-Path $out 'rpf-source.dds'
$edited = Join-Path $out 'rpf-edit.dds'
$copy = Join-Path $out 'core_restofworld-test.rpf'
& $cli export $catalog $target $untouched
$bytes = [IO.File]::ReadAllBytes($untouched)
$bytes[152] = $bytes[152] -bxor 1
[IO.File]::WriteAllBytes($edited, $bytes)
& $cli write-rpf-copy $catalog $target $edited $copy

$copyCatalog = Join-Path $out 'rpf-copy.json'
& $cli catalog $copyCatalog --fresh --inventory $index $copy
& $cli export $copyCatalog $target (Join-Path $out 'rpf-reopened.dds')
Get-FileHash $edited, (Join-Path $out 'rpf-reopened.dds')
& $cli verify $catalog
```

Expect matching hashes for the edited and re-exported DDS. Passing the untouched
DDS to `write-rpf-copy` with a fresh destination reproduces the original archive
SHA-256. The copy preserves the archive size and directory bytes.

</details>

## License

ViceModVI's own code is released under the [GNU GPL v3 or later](LICENSE)
(`GPL-3.0-or-later`). Game files retain their respective rights.
