using System.Text.Json;
using RpfPreview.Core;
using ViceModVI;

try
{
    return Run(args);
}
catch (Exception error) when (error is IOException or InvalidDataException or ArgumentException
    or InvalidOperationException or JsonException or NotSupportedException or FormatException or OverflowException
    or KeyNotFoundException or UnauthorizedAccessException)
{
    Console.Error.WriteLine(error.Message);
    return 1;
}

static void Print(object result) => Console.WriteLine(JsonSerializer.Serialize(result, LocalFiles.Json));

static int Run(string[] args)
{
    switch (args)
    {
        case ["inventory-tsv", var index, var inventory]:
            XboxAssets.ConvertCarveIndex(index, inventory);
            return 0;
        case ["catalog", var output, .. var inputs] when inputs.Length > 0:
        {
            bool useCache = inputs[0] != "--fresh";
            if (!useCache) inputs = inputs[1..];
            string? inventory = null;
            if (inputs is ["--inventory", var path, .. var rest])
            {
                inventory = path;
                inputs = rest;
            }
            var catalog = XboxAssets.Build(inputs.SelectMany<string, string>(input => Directory.Exists(input)
                ? Directory.EnumerateFiles(input, "*", SearchOption.AllDirectories)
                    .Where(file => Path.GetExtension(file).ToLowerInvariant() is ".rpf" or ".ttpf")
                    .Order(StringComparer.OrdinalIgnoreCase)
                : new[] { input }), inventory, useCache: useCache);
            foreach (var skipped in catalog.Skipped)
                Console.Error.WriteLine($"Skipped {skipped.Path}: {skipped.Reason}");
            catalog.Save(output);
            Print(catalog.Assets.GroupBy(asset => asset.Type).ToDictionary(group => group.Key, group => group.Count()));
            return 0;
        }
        case ["import-manifest", var file, var sourceId, var manifest]:
        {
            var catalog = Catalog.Load(file);
            var result = Manifests.Import(catalog, sourceId, manifest);
            catalog.Save(file, replace: true);
            Print(result);
            return 0;
        }
        case ["list", var file, .. var filters]:
        {
            var options = new Dictionary<string, string>();
            if (filters.Length % 2 != 0) throw new ArgumentException("Filters require a value.");
            for (int i = 0; i < filters.Length; i += 2)
            {
                if (filters[i] is not ("--source" or "--type" or "--name" or "--hash" or "--offset"))
                    throw new ArgumentException($"Unknown filter {filters[i]}");
                options.Add(filters[i], filters[i + 1]);
            }
            var catalog = Catalog.Load(file);
            Print(catalog.Find(options.GetValueOrDefault("--source"), options.GetValueOrDefault("--type"),
                options.GetValueOrDefault("--name"), options.GetValueOrDefault("--hash"),
                options.TryGetValue("--offset", out var offset) ? Catalog.Number(offset) : null));
            return 0;
        }
        case ["read", var file, var id, var output, .. var extra] when extra.Length <= 1:
        {
            var catalog = Catalog.Load(file);
            ReadMeasurement? measure = null;
            LocalFiles.Write(output, stream => measure = catalog.CopyStored(catalog.Get(id), stream, extra.SingleOrDefault()));
            Print(measure!);
            return 0;
        }
        case ["verify", var file]:
        {
            var catalog = Catalog.Load(file);
            var results = catalog.Sources.Select(source =>
            {
                using var stream = source.Open();
                return new { source.Id, source.Fingerprint.Sha256, Bytes = source.Fingerprint.Check(source.Path, stream, true) };
            }).ToArray();
            Print(results);
            return 0;
        }
        case ["export", var file, var id, var output]:
        {
            var catalog = Catalog.Load(file);
            Print(XboxAssets.Export(catalog, catalog.Get(id), output));
            return 0;
        }
        case ["resolve", var file, .. var ids] when ids.Length <= 1:
        {
            var catalog = Catalog.Load(file);
            var models = ids.Length == 1 ? new[] { catalog.Get(ids[0]) } : catalog.Find(type: "model").ToArray();
            var links = models.SelectMany(asset => TextureLinks.Resolve(catalog, asset, XboxAssets.ReadModel(catalog, asset))
                .Select(link => new { Model = asset.Id, Link = link })).ToArray();
            Print(new { Counts = new[] { "confirmed", "ambiguous", "unresolved" }.ToDictionary(status => status,
                status => links.Count(row => row.Link.Status == status)), Links = links });
            return 0;
        }
        case ["write-pool-copy", var file, var id, var input, var output, .. var options]:
        {
            if (options.Any(option => option is not ("--overwrite" or "--resize")) || options.Distinct().Count() != options.Length)
                throw new ArgumentException("Supported pool copy flags are --overwrite and --resize.");
            if (string.Equals(Path.GetFullPath(output), Path.GetFullPath(file), StringComparison.OrdinalIgnoreCase))
                throw new IOException("Choose a destination separate from the catalog.");
            var catalog = Catalog.Load(file);
            var result = PoolWriter.Copy(catalog, catalog.Get(id), TextureConversion.Import(input), output,
                overwriteExisting: options.Contains("--overwrite"), allowResize: options.Contains("--resize"));
            if (result.ReplacedSource) catalog.Save(file, replace: true);
            Print(result);
            return 0;
        }
        case ["convert-for-ttpf", var file, var id, var input, var output, .. var options]
            when options is [] or ["--resize"]:
        {
            var catalog = Catalog.Load(file);
            var dds = TextureConversion.Import(input);
            var plan = PoolWriter.Plan(catalog, catalog.Get(id), dds);
            var prepared = TextureConversion.Apply(dds, plan, allowResize: options.Length == 1);
            LocalFiles.Write(output, stream => stream.Write(prepared));
            Print(plan);
            return 0;
        }
        case ["write-rpf-copy", var file, var id, var input, var output, .. var options]
            when options is [] or ["--resize"]:
        {
            var catalog = Catalog.Load(file);
            Print(RpfCopyWriter.Copy(catalog, catalog.Get(id), TextureConversion.Import(input), output,
                allowResize: options.Length == 1));
            return 0;
        }
        case ["pool-profiles", var file, var sourceId]:
        {
            var source = Catalog.Load(file).Sources.Single(item => item.Id == sourceId && item.Format == "ttpf");
            using var stream = source.Open();
            source.Fingerprint.Check(source.Path, stream, false);
            Print(TtpfReader.Inspect(source.Path).Textures.Select(t => new { AssetId = $"{source.Id}:ttpf:{t.Index}",
                t.Index, t.Name, t.DxgiFormat, t.Width, t.Height, t.Mips, t.SourceMips, t.SupportsNativeReplacement, t.Limitation }));
            return 0;
        }
        case ["inspect", var path] when Path.GetExtension(path).Equals(".dds", StringComparison.OrdinalIgnoreCase):
        {
            var dds = DdsTextureReader.Inspect(File.ReadAllBytes(path));
            Print(new { dds.Width, dds.Height, dds.Mips, dds.DxgiFormat, dds.SliceCount, dds.DataSize });
            return 0;
        }
        case ["inspect", var path] when Path.GetExtension(path).Equals(".obj", StringComparison.OrdinalIgnoreCase):
        {
            var mesh = ObjModelReader.Parse(File.ReadAllText(path));
            Print(new { Vertices = mesh.Meshes.Sum(part => part.Positions.Count),
                Faces = mesh.Meshes.Sum(part => part.Indices.Count) / 3, Meshes = mesh.Meshes.Count });
            return 0;
        }
        default:
            Console.Error.WriteLine("""
                inventory-tsv LEONIDA_INDEX_JSON INVENTORY_TSV
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
                pool-profiles CATALOG SOURCE_ID
                """);
            return 2;
    }
}
