# Pending work

Choose a task that can be closed with the listed evidence. Tasks are ordered by dependency. The team can prepare exporters and format tests in parallel while agreeing on the catalog and reference contracts.

| ID | Task | Depends on | Status |
| --- | --- | --- | --- |
| MF-01 | Catalog and byte-offset reads | — | Completed |
| MF-02 | Identify and export models and textures | MF-01 for integration | Completed |
| MF-03 | Link materials and textures | MF-01, MF-02 | Completed |
| MF-04 | Viewer and replacement project | MF-02, MF-03 | Completed |
| MF-05 | Experimental TTPF copy writer | MF-02 for format validation, MF-04 for integration | Completed |
| MF-06 | RPF9 writing and change distribution | Native metadata and in-game verification per platform | Blocked |

## MF-01 · Catalog and byte-offset reads

Import an RPF inventory TSV and manifests containing names, types, and locators. Use an inventory of `core_restofworld.rpf` as the sample. Separate the byte range from asset identity. For an RPF, retain the container, offset, length, and any segments. For a TTPF, retain the container, native key, and local index when available. Bind the catalog to a verifiable fingerprint of the file. Support filtering by source, type, name, or hash.

**Acceptance.** A second open can retrieve an entry from the saved catalog without rescanning the container. Record the bytes read for the operation and show that an out-of-file range and a catalog belonging to a different copy are rejected.

**Evidence.** On Xbox Series build 1.0.2.1, Leonida's inventory yielded 2,381 stored ranges, tracked separately from 37 models, 145 dictionaries, and 847 indexed textures across the RPF and five pools. The Leonida manifests matched 37/37 model and 194/194 TTPF locators. A second process reading `0:range:4010` transferred 162 payload bytes and 32,768 fingerprint bytes from the 1,295,760,752-byte RPF. Moving that locator to EOF or pointing the read at a different container returned an error and produced no output. An alternate synthetic source with the same length and modification time failed the full SHA-256 check. Full SHA-256 verification matched all six real sources against Leonida's build profile. [Catalog reproduction](README.md#reproduce-the-sample)

## MF-02 · Identify and export

Connect existing readers to catalog entries. Compare the output with existing manifests. Validate the type from the asset's structure. Retain `unknown` when only a zlib block is available or validation fails. For textures, record dimensions, format, source mips, and exported mips. For models, record meshes, materials, and locators. Expose per-entry reading through an interface the viewer can reuse.

**Acceptance.** Export a reference texture and mesh from the catalog. Reopen each output and verify texture dimensions and available mips, or vertex and face counts for the mesh. Identify the decoder used and distinguish complete outputs from partial ones.

**Evidence.** `RageModelDecoder` exported `0:model:203F010` with 24 vertices, 12 triangles, two meshes, and two source materials. Reopening the OBJ gave the same vertex, triangle, and mesh counts as the Leonida manifest. OBJ covers static geometry and UVs. `TtpfReader` exported `3:ttpf:0` as a 512 × 512 DXGI 71 DDS with 3 of 8 source mips. The reopened header matched the TTPF manifest. The OBJ and DDS both matched Leonida's `rpf9dump` exports byte for byte. Both decoders use the same pinned rpf9-explorer Core revision. [Setup instructions](README.md#build)

## MF-03 · Resolve materials and textures

Start with an exported mesh. Use material references and check embedded textures, RPF dictionaries, and TTPF pools. Preserve full identifiers, provenance, and evidence for every link. A hash may have multiple candidates. When the reference evidence is insufficient, show the candidates and explain the uncertainty.

**Acceptance.** Select the material of a reference mesh and reach its diffuse texture through a reproducible association. Include an ambiguous case that remains unresolved. Measure confirmed, ambiguous, and unresolved links separately.

**Evidence.** Shader 1 on `0:model:203F010` uses `diffusetex` (parameter `C8E8C282`). Its 64-bit key `6B455BAEAE3DA9F4` matches entry 90 in `ttdb_props.ttpf`. Across 101 recognized color references, 23 were confirmed, zero ambiguous, and 78 unresolved. Adding a local duplicate of the props pool returned two candidates for the reference shader and left its selected texture empty. See [sample commands](README.md#reproduce-the-sample).

## MF-04 · Viewer and local project

Show the catalog, texture preview, mesh and material selection, and per-entry export. Accept a replacement and report differences in dimensions, format, and mips as applicable. Support before-and-after comparison and save the state outside the source file. Record the identity of the replaced entry.

**Acceptance.** A second contributor can open the sample, edit a texture, see it on the model, close and reopen the project, and revert the change. Check the original files' fingerprints before and after the workflow.

**Evidence.** In the [local replacement workflow](README.md#local-replacement-workflow), the WPF scene applied the 1,024 × 1,024 source image to the reference mesh's shader 1. A 64 × 64 test PNG changed the displayed mesh texture and reported the dimension, DXGI format, and mip differences. The desktop integration check toggled both images, saved and closed the window, and reopened the project with the edited image selected. It verified the persisted native key and confirmed the revert remained saved. All six original SHA-256 values matched before and after.

## MF-05 · TTPF copy with one replacement

Start with one texture in a readable storage profile. Document the conditions for accepting a replacement. Write a new copy, update the fields required by the profile, and reopen it with a reader. Verify the modified texture and a neighboring entry. Reject profiles unsupported by the writer.

**Acceptance.** The copy remains structurally readable, the expected texture can be exported again, and the neighboring entry retains its content. Record in-game compatibility as pending until verified on the exact platform and version.

**Evidence.** In the [experimental copy workflow](README.md#experimental-ttpf-copy), `textures_loose.ttpf` entry 4, key `05929C00601B8338`, accepted a DXGI 98 DDS at 1,024 × 1,024 with all 9 mips. Changing one decoded byte rewrote one chunk and increased the physical size from 21,081,856 to 21,082,112 bytes. A fresh catalog of the copy reopened all 194 descriptors. The re-exported replacement and neighbor 3 matched their expected DDS SHA-256 values. The writer rejected entry 0's partial storage profile and produced no copy. Xbox Series 1.0.2.1 in-game compatibility remains pending.

## MF-06 · RPF9 writing and distribution

Document which RPF9 metadata remains uninterpreted and what a writer needs to produce a valid copy. Design replacement packages once the platform's supported application mechanism is known.

**Acceptance per platform.** Apply and revert a reproducible change on a copy and verify that the game reads the result. Each platform requires its own test and compatibility record.

**Research evidence.** The Xbox Series 1.0.2.1 sample has a root `9FPR` header and five observed nested headers. The root table spans `0x40–0xAA0` with its end excluded. The words at `0x08` and `0x0C` delimit internal regions whose record layouts remain unverified. The root word at `0x24` is 39, while the five observed nested headers and the recovered payload counts give different totals. Resource recovery locates zlib segments and reconstructs validated assets. The original path and member records, archive nesting rules, and integrity fields remain unresolved. The external Core reader likewise reports an undecoded native directory. [Pinned rpf9-explorer source](https://github.com/breadbbbb/rpf9-explorer/tree/48557ead00601d3757ba12b894f2595c4faad2a5/Core)

An Xbox writer needs the verified member and directory layouts, offsets and lengths, compression and alignment requirements, and any integrity metadata affected by a replacement. It needs a supported way to apply a copy on the target platform. Validation requires reopening a no-change round trip, applying one identifiable replacement to a fresh copy, checking an unaffected entry, and reverting by reapplying the original. Record both archive fingerprints and the exact platform and game build used for the runtime check.

| Platform | Build | Compatibility |
| --- | --- | --- |
| Xbox Series | 1.0.2.1 | Pending native writer and a runnable platform test |
| PS5 | — | Pending a format profile and a platform test |
| PC | — | Pending a format profile and a platform test |

## Reporting progress

Update task status in the same change that provides evidence. State the sample version, reproduction command or steps, and expected and observed results. Give counts with denominators and units. Detected assets, decoded models, and resolved references are separate measures. The number of scanned bytes cannot establish the number of recovered names, paths, or assets.

[Project overview](README.md) · [Contribution guide](CONTRIBUTING.md)
