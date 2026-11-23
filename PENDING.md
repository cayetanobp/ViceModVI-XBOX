# Pending work

Choose a task that can be closed with the listed evidence. Tasks are ordered by dependency. The team can prepare exporters and format tests in parallel while agreeing on the catalog and reference contracts.

| ID | Task | Depends on | Status |
| --- | --- | --- | --- |
| MF-01 | Catalog and byte-offset reads | — | Completed |
| MF-02 | Identify and export models and textures | MF-01 for integration | Completed |
| MF-03 | Link materials and textures | MF-01, MF-02 | Completed |
| MF-04 | Viewer and replacement project | MF-02, MF-03 | Completed |
| MF-05 | Experimental TTPF copy writer | MF-02 for format validation, MF-04 for integration | Completed |
| MF-06 | RPF9 writing and change distribution | Native metadata and in-game verification per platform | Offline copy verified, Xbox test pending |
| MF-07 | Simplify browsing, previews, exports, and edits | MF-02, MF-03, MF-04 | Completed |
| MF-08 | Installation root and incremental global index | MF-01, MF-03 | Pending |
| MF-09 | Prebuilt index packs by platform and build | MF-08 | Proposed |
| MF-10 | Rust port parity on the reference sample | MF-01 to MF-07 | Synthetic tests pass, sample run pending |

## MF-01 · Catalog and byte-offset reads

Import an RPF inventory TSV and manifests containing names, types, and locators. Use an inventory of `core_restofworld.rpf` as the sample. Separate the byte range from asset identity. For an RPF, retain the container, offset, length, and any segments. For a TTPF, retain the container, native key, and local index when available. Bind the catalog to a verifiable fingerprint of the file. Support filtering by source, type, name, or hash.

**Acceptance.** A second open can retrieve an entry from the saved catalog without rescanning the container. Record the bytes read for the operation and show that an out-of-file range and a catalog belonging to a different copy are rejected.

**Evidence.** On Xbox Series build 1.0.2.1, Leonida's inventory yielded 2,381 stored ranges, tracked separately from 37 models, 145 dictionaries, and 847 indexed textures across the RPF and five pools. The Leonida manifests matched 37/37 model and 194/194 TTPF locators. A second process reading `0:range:4010` transferred 162 payload bytes and 32,768 fingerprint bytes from the 1,295,760,752-byte RPF. Moving that locator to EOF or pointing the read at a different container returned an error and produced no output. An alternate synthetic source with the same length and modification time failed the full SHA-256 check. Full SHA-256 verification matched all six real sources against Leonida's build profile. [Catalog reproduction](README.md#reproduce-the-sample)

## MF-02 · Identify and export

Connect existing readers to catalog entries. Compare the output with existing manifests. Validate the type from the asset's structure. Retain `unknown` when only a zlib block is available or validation fails. For textures, record dimensions, format, source mips, and exported mips. For models, record meshes, materials, and locators. Expose per-entry reading through an interface the viewer can reuse.

**Acceptance.** Export a reference texture and mesh from the catalog. Reopen each output and verify texture dimensions and available mips, or vertex and face counts for the mesh. Identify the decoder used and distinguish complete outputs from partial ones.

**Evidence.** Recorded with the earlier C# build: the drawable decoder exported `0:model:203F010` with 24 vertices, 12 triangles, two meshes, and two source materials. Reopening the OBJ gave the same vertex, triangle, and mesh counts as the Leonida manifest. OBJ covers static geometry and UVs. The pool reader exported `3:ttpf:0` as a 512 × 512 DXGI 71 DDS with 3 of 8 source mips. The reopened header matched the TTPF manifest. The OBJ and DDS both matched Leonida's `rpf9dump` exports byte for byte. [MF-10](#mf-10--rust-port-parity-on-the-reference-sample) tracks repeating this with the current readers. [Setup instructions](README.md#build)

## MF-03 · Resolve materials and textures

Start with an exported mesh. Use material references and check embedded textures, RPF dictionaries, and TTPF pools. Preserve full identifiers, provenance, and evidence for every link. A hash may have multiple candidates. When the reference evidence is insufficient, show the candidates and explain the uncertainty.

**Acceptance.** Select the material of a reference mesh and reach its diffuse texture through a reproducible association. Include an ambiguous case that remains unresolved. Measure confirmed, ambiguous, and unresolved links separately.

**Evidence.** Shader 1 on `0:model:203F010` uses `diffusetex` (parameter `C8E8C282`). Its 64-bit key `6B455BAEAE3DA9F4` matches entry 90 in `ttdb_props.ttpf`. Across 101 recognized color references, 23 were confirmed, zero ambiguous, and 78 unresolved. Adding a local duplicate of the props pool returned two candidates for the reference shader and left its selected texture empty. See [sample commands](README.md#reproduce-the-sample).

## MF-04 · Viewer and local project

Show the catalog, texture preview, mesh and material selection, and per-entry export. Accept a replacement and report differences in dimensions, format, and mips as applicable. Support before-and-after comparison and save the state outside the source file. Record the identity of the replaced entry.

**Acceptance.** A second contributor can open the sample, edit a texture, see it on the model, close and reopen the project, and revert the change. Check the original files' fingerprints before and after the workflow.

**Evidence.** In the [local replacement workflow](README.md#local-replacement-workflow), the WPF scene applied the 1,024 × 1,024 source image to the reference mesh's shader 1. A 64 × 64 test PNG changed the displayed mesh texture and reported the dimension, DXGI format, and mip differences. The desktop integration check toggled both images, saved and closed the window, and reopened the project with the edited image selected. It verified the persisted native key and confirmed the revert remained saved. All six original SHA-256 values matched before and after.

**Current desktop workflow.** MF-07 replaced the project menus with session edits and DDS export. The **Convert for TTPF export** dialog previews input and output dimensions, DXGI format, and mip counts. It explicitly asks for a decision when resizing is required and leaves the staged DDS available for both preview and export. [Reproduction steps](README.md#local-replacement-workflow)

## MF-05 · TTPF copy with one replacement

Start with one texture in a readable storage profile. Document the conditions for accepting a replacement. Write a new copy, update the fields required by the profile, and reopen it with a reader. Verify the modified texture and a neighboring entry. Reject profiles unsupported by the writer.

**Acceptance.** The copy remains structurally readable, the expected texture can be exported again, and the neighboring entry retains its content. Record in-game compatibility as pending until verified on the exact platform and version.

**Evidence.** In the [experimental copy workflow](README.md#experimental-ttpf-copy), `textures_loose.ttpf` entry 4, key `05929C00601B8338`, accepted a DXGI 98 DDS at 1,024 × 1,024 with all 9 mips. Changing one decoded byte rewrote one chunk and increased the physical size from 21,081,856 to 21,082,112 bytes. A fresh catalog of the copy reopened all 194 descriptors. The re-exported replacement and neighbor 3 matched their expected DDS SHA-256 values. The writer rejected entry 0's partial storage profile and produced no copy. Xbox Series 1.0.2.1 in-game compatibility remains pending.

**Conversion evidence.** Entry 4 passed a compatible DDS through byte for byte. A 1,024 × 1,024 DXGI 91 DDS with one mip was written as a verified DXGI 98 copy with nine mips. The mip-only BC7 case preserved the supplied mip bytes and added eight levels. JPEG, BMP, GIF, TIFF, WebP and TGA fixtures imported as one-level BGRA DDS images. A 64 × 64 PNG required `--resize`. With consent, the reopened TTPF matched all 1,398,096 bytes of the standalone converted DDS payload and passed the neighbor check. Entry 0 reported its three readable mips out of eight and produced no output. [Conversion and copy commands](README.md#experimental-ttpf-copy)

## MF-06 · RPF9 writing and distribution

Document which RPF9 metadata remains uninterpreted and what a writer needs to produce a valid copy. Design replacement packages once the platform's supported application mechanism is known.

**Acceptance per platform.** Apply and revert a reproducible change on a copy and verify that the game reads the result. Each platform requires its own test and compatibility record.

**Research evidence.** The Xbox Series 1.0.2.1 sample has a root `9FPR` header and five observed nested headers. The root table spans `0x40–0xAA0` with its end excluded. The words at `0x08` and `0x0C` delimit internal regions whose record layouts remain unverified. The root word at `0x24` is 39, while the five observed nested headers and the recovered payload counts give different totals. Resource recovery locates zlib segments and reconstructs validated assets. The original path and member records, archive nesting rules, and integrity fields remain unresolved.

An Xbox writer needs the verified member and directory layouts, offsets and lengths, compression and alignment requirements, and any integrity metadata affected by a replacement. It needs a supported way to apply a copy on the target platform. Validation requires reopening a no-change round trip, applying one identifiable replacement to a fresh copy, checking an unaffected entry, and reverting by reapplying the original. Record both archive fingerprints and the exact platform and game build used for the runtime check.

**Offline copy evidence.** A no-change round trip reproduced the 1,295,760,752-byte archive's original SHA-256. A one-pixel edit of `0:dictionary:680010:texture:0` changed one segment at `0x6800B0` and preserved its 36-byte packed length. The output SHA-256 was `17D09D966D0E4DAC63C2865BBEEC56EFBC4C25D362758814D1FAAAA0BF50B180`. The reopened dictionary matched the replacement DDS and its neighbor, and bytes outside that segment matched throughout the copy. Rebuilding the catalog of the copy re-exported the exact DDS. A DDS requiring a different packed length was rejected without producing a copy. [RPF9 copy commands](README.md#experimental-rpf9-copy)

| Platform | Build | Compatibility |
| --- | --- | --- |
| Xbox Series | 1.0.2.1 | Fixed-size dictionary copy verified offline. Loading and reverting in the game are pending |
| PS5 | — | Pending a format profile and a platform test |
| PC | — | Pending a format profile and a platform test |

## MF-07 · Browse, preview, export, and edit in one place

Start with **Open folder** and **Open file**. Show a tree of installed folders, archives, and their contents wherever paths are known. Present recovered entries with stable names or offsets when an archive does not expose its directory. Keep the source path visible as the user moves between archives. Add search by name, type, and native key, and show where each match lives.

Selecting an entry opens its preview. A model shows its meshes and materials, with links to confirmed textures in either the same archive or another folder. Put single and batch export in the selection's context menu. For supported textures, use an explicit edit mode with replacement preview, validation, and undo before saving a copy. Keep the existing command-line workflow available.

**Acceptance.** Starting from an installed game, a contributor can navigate to an archive, preview a model, follow a texture reference into a different folder, export the selected assets, and stage and revert a texture edit. Opening or browsing asks for no catalog filename or output directory. Export and save ask for a destination only when they write output. Opening a standalone file remains available without configuring an installation.

**Evidence.** The desktop integration check opened the local Xbox Series 1.0.2.1 sample folder, navigated to `core_restofworld.rpf`, and previewed model `0:model:203F010` using shader 1. **View linked texture** selected the resolved entry in `data/ttdb/props/ttdb_props.ttpf`. Native-key search returned the same entry. The check exercised the two-asset context selection and reopened exported OBJ and DDS files. It staged and reverted an image on a pool entry with a partial native mip chain, then verified the conversion plan on a writable entry and opened its pool as a standalone file. Opening requested no output filename. [Desktop walkthrough](README.md#local-replacement-workflow)

### Desktop texture round trip

With the Windows .NET 10 Release build and the Xbox Series 1.0.2.1 sample, export RPF9 texture `0:dictionary:680010:texture:0` and TTPF texture `3:ttpf:4` as DDS. For the 4 × 4 BC1 texture, XOR the byte at file offset 152 with `1`. For the 1,024 × 1,024 BC7 texture, reverse the order of the 256 blocks in each block row of the first mip, using 16 bytes per block from file offset 148. This preserves the eight lower mips. Import each DDS with **Edit texture** and confirm the preview changes. **Revert replacement** should restore the original. [Desktop import steps](README.md#local-replacement-workflow)

Use the CLI to [write an RPF9 copy](README.md#experimental-rpf9-copy) and [write a TTPF copy](README.md#experimental-ttpf-copy). Reopen both copies in the viewer, rebuild each catalog, and export the same texture to compare its SHA-256 with the edited DDS.

The RPF9 edit changed one visible pixel and rewrote one segment. The TTPF edit changed 515,510 pixels in the first mip and rewrote one chunk. The viewer rendered both edits from the reopened copies, the re-exported DDS hashes matched, and both writers verified a neighboring entry. The original RPF and TTPF SHA-256 hashes matched before and after the test. In-game verification remains pending.

## MF-08 · Installation root and incremental global index

Prompt once for the game root when the user sets up an installation. Read its manifest or configuration to identify the platform, package identity, and build. Persist the root and keep its index separate from the temporary catalog used for the folder currently open. The folder sets the view. The installation index supplies cross-folder texture and other dependency lookups.

Build and save the inventory one container at a time. Keep locators for known resources and for unclassified ranges, including relative path, offsets, lengths, and available signatures. Track the versions of inventory and format readers independently so a new reader can revisit candidate ranges without throwing away established model and texture records. Save finished containers atomically, report progress by phase, and resume at the first container that still needs work.

**Acceptance.** Interrupt a first scan and reopen the installation. Finished containers are reused. Opening a subfolder still resolves a model's confirmed texture from a separate folder. Changing one container only invalidates that container. Adding a reader can reclassify candidate ranges in an unchanged container from the saved inventory.

## MF-09 · Import a prebuilt index by platform and build

Prepare optional index packs for verified releases. Key each pack by platform, package identity, build, and index schema. Store relative container paths, locators, reader versions, and reference fingerprints. Keep the packs free of game files and extracted content.

On first setup, look up an index pack from the local manifest. Treat a manifest match as a starting point and check each local container against the pack before relying on offsets. Start with a cheap comparison using file size and sampled bytes. Use a stronger check when a container is suspect and verify the full source fingerprint before a write. Import matching entries, scan containers that are new or differ, and mark missing containers unavailable. An unknown build can still open and build its own index. Track pack provenance and compare results across versions before introducing any cross-version reuse.

**Acceptance.** Use an index pack on a matching build and compare the resulting catalog with a full local scan. Repeat with one changed container, one new container, and one missing container. Record which containers were reused or scanned and compare the time to first usable view against a fresh scan.

## MF-10 · Rust port parity on the reference sample

The C# and WPF build was replaced by Rust crates and an egui viewer. The
evidence in MF-01 to MF-07 was recorded with the C# build. Repeat it with the
Rust build on the **Xbox Series 1.0.2.1** sample.

**Done when.** The [sample commands](README.md#reproduce-the-sample) produce the
same asset counts and identifiers as a catalog written by the C# build, the
DDS and OBJ exports match byte for byte, `write-rpf-copy` with the untouched
DDS reproduces the original SHA-256, and the TTPF and RPF9 copies reopen with
the edited texture. Record the catalog build time of both builds on the same
machine, uncached and cached.

**Known differences.** Previews decode BC blocks with `bcdec_rs` instead of
BCnEncoder.Net, so a few preview pixels can differ by one step. Conversions
encode BC1 to BC5 with `texpresso` and BC7 with `block_compression`, and
resample without ImageMagick, so converted DDS bytes differ from the C# build.
Native DDS exports and verified copies do not depend on these libraries. OBJ
files write floats in the shortest round-trip form without exponents. The
source index cache moved to revision 3, so the first open after upgrading
indexes each source again.

## Reporting progress

Update task status in the same change that provides evidence. State the sample version, reproduction command or steps, and expected and observed results. Give counts with denominators and units. Detected assets, decoded models, and resolved references are separate measures. The number of scanned bytes cannot establish the number of recovered names, paths, or assets.

[Project overview](README.md) · [Contribution guide](CONTRIBUTING.md)
