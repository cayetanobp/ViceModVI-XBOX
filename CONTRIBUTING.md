# Contributing to ViceModVI

The unit of work is an `MF-XX` task in [PENDING.md](PENDING.md). Each contribution includes what another person needs to review and reproduce it independently.

## Set up the environment

1. Work from a local copy of the repository and choose a task whose dependencies are available. See [README.md](README.md) for the initial scope.
2. Keep sample containers and analysis outputs outside the repository. Use small synthetic fixtures with known results for automated tests. Validate real cases with a copy you can access.
3. For the first real case, use an Xbox Series `core_restofworld.rpf` and its `index.tsv` if available. For cross-file relationships, add the TTPF containing the texture being checked. Pass all paths through arguments or local configuration.
4. When a task introduces the first executable or library in its area, document the dependencies and the exact build and reproduction commands in the same change. The project starts with documentation. Choose the build tool with the first implementation that needs it.

## Implement

- For the catalog and readers, first check which fields the TSV inventories already supply and which fields the available export manifests contain. Reuse those outputs through an importer or a simple interface before implementing another scanner.
- Record the provenance of each value. Retain the offset, length, native identifier, and evidence type when available. Label aliases and candidate associations. Store 64-bit identifiers as hexadecimal strings in JSON to avoid precision loss in clients using floating-point numbers.
- For editing, keep the source read-only and write replacements to the project or to a new copy. Include what happens when that copy is reopened in the evidence.
- When incorporating third-party code, check the license before combining it with the project and retain required attributions and notices. When interoperating with external tools, document their version, input format, and interface.
- Keep Xbox format logic in its backend. Put shared catalog, project, and view functionality in platform-independent code. Document the profile and version tested.

## Submit a reviewable change

If using Git, use a recognizable branch name such as `mf-03-material-texture`. Reference the task in the PR and copy this template into the description.

```markdown
Task: MF-XX
Changes:
Input: platform, version, sample name, and fingerprint if available
Reproduction: tools, versions, and complete command or steps
Expected result:
Observed result:
Measurement: counts, positive cases, and rejected or ambiguous cases
Remaining limitations:
```

Use examples and relative paths that others can adapt. For game samples, attach only shareable metrics, steps, and diagnostic output. Keep RPF and TTPF files, executables, keys, extracted assets, generated game catalogs, and local binary results outside version control.

## Review and close

1. Check the task acceptance criteria using its documented reproduction steps.
2. Review a positive case and, where applicable, an invalid range, unsupported format, or ambiguous relationship.
3. After exporting or writing a file, reopen it and record the fields or content compared. Distinguish viewer reads from in-game tests.
4. Update the task and document open cases that could change the result. If independent work comes up, add it to [PENDING.md](PENDING.md) with the expected evidence.
