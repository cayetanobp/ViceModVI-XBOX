using System.IO.Compression;
using System.Text.Json;
using RpfPreview.Core;

namespace ViceModVI;

public sealed record MeshExport(int Vertices, int Faces, int Meshes, int Materials, string Decoder, string Coverage);
public sealed record TextureExport(int Width, int Height, int DxgiFormat, int SourceWidth, int SourceHeight, int SourceFormat,
    int SourceMips, int ExportedMips, int BaseMip, string Decoder, string Coverage);

public sealed record IndexProgress(string Phase, long Completed, long Total, string Unit);

public static class XboxAssets
{
    public const string ExplorerRevision = "48557ead00601d3757ba12b894f2595c4faad2a5";
    private const int MaxResourceBytes = 256 * 1024 * 1024;

    public static Catalog Build(IEnumerable<string> files, string? inventory = null,
        Action<string, IndexProgress>? progress = null, bool useCache = true,
        CancellationToken cancellationToken = default)
    {
        var catalog = new Catalog();
        string[] paths = files.Select(Path.GetFullPath).Distinct(StringComparer.OrdinalIgnoreCase).ToArray();
        bool imported = false;
        for (int i = 0; i < paths.Length; i++)
        {
            cancellationToken.ThrowIfCancellationRequested();
            string path = paths[i];
            Action<IndexProgress>? report = progress is null ? null : entry => progress(path, entry);
            bool cache = useCache && inventory is null;
            if (cache && SourceCatalogCache.TryRestore(catalog, path)) continue;
            int firstSource = catalog.Sources.Count, firstAsset = catalog.Assets.Count;
            try
            {
                AddSource(catalog, path, inventory, ref imported, report, cache, cancellationToken);
            }
            // One unreadable source (a zeroed header, a cut-off file, a reader limit) is reported and
            // left out; the folder still opens and completed sources stay cached. An explicit
            // inventory names a single RPF, so its failures are not hidden.
            catch (Exception error) when (inventory is null && IsSourceError(error))
            {
                catalog.Assets.RemoveRange(firstAsset, catalog.Assets.Count - firstAsset);
                catalog.Sources.RemoveRange(firstSource, catalog.Sources.Count - firstSource);
                catalog.Skipped.Add(new(path, error.Message));
                report?.Invoke(new($"Skipped: {error.Message}", 0, 0, "entries"));
            }
        }
        if (inventory is not null && !imported) throw new InvalidDataException("An inventory requires an RPF source.");
        if (catalog.Sources.Count == 0)
            throw new InvalidDataException(catalog.Skipped.Count == 0 ? "No RPF or TTPF sources found." :
                $"No source could be read. {string.Join(" ", catalog.Skipped.Select(s => $"{Path.GetFileName(s.Path)}: {s.Reason}"))}");
        return catalog;
    }

    /// Errors that mean one source cannot be indexed, as opposed to cancellation or a bug.
    internal static bool IsSourceError(Exception error) => error is IOException or InvalidDataException or
        UnauthorizedAccessException or NotSupportedException or FormatException or OverflowException or
        ArgumentException or IndexOutOfRangeException or InvalidOperationException or JsonException or
        System.ComponentModel.Win32Exception;

    private static void AddSource(Catalog catalog, string path, string? inventory, ref bool imported,
        Action<IndexProgress>? report, bool cache, CancellationToken cancellationToken)
    {
        int firstAsset = catalog.Assets.Count;
        string format = Path.GetExtension(path).ToLowerInvariant() switch
        {
            ".rpf" => "rpf9",
            ".ttpf" => "ttpf",
            _ => throw new InvalidDataException($"Unsupported source: {path}")
        };
        using var input = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read);
        Span<byte> magic = new byte[4];
        input.ReadExactly(magic);
        if (!(format == "rpf9" ? magic.SequenceEqual("9FPR"u8) : magic.SequenceEqual("RDS5"u8)))
            throw new InvalidDataException($"Unexpected {format} signature: {path}");
        long length = input.Length;
        report?.Invoke(new("Reading source fingerprint", 0, length, "bytes"));
        var fingerprint = Fingerprint.Create(path, input,
            report is null ? null : bytes => report(new("Reading source fingerprint", bytes, length, "bytes")), cancellationToken);
        var source = new Source(catalog.Sources.Count.ToString(), path, format, fingerprint);
        catalog.Sources.Add(source);
        if (format == "ttpf")
        {
            report?.Invoke(new("Reading texture table", 0, 0, "entries"));
            var textures = TtpfReader.Inspect(path, cancellationToken).Textures;
            int read = 0;
            foreach (var texture in textures)
            {
                cancellationToken.ThrowIfCancellationRequested();
                catalog.Assets.Add(new($"{source.Id}:ttpf:{texture.Index}", source.Id, "texture", texture.Name,
                    "alias", "RDS5/ETPF descriptor", Index: texture.Index, NativeKey: Key(texture.AssetKey)));
                if (++read % 128 == 0 || read == textures.Count)
                    report?.Invoke(new("Indexing textures", read, textures.Count, "entries"));
            }
            if (cache) SourceCatalogCache.Store(catalog, source, firstAsset);
            return;
        }
        IReadOnlyList<long>? offsets = null;
        if (inventory is not null)
        {
            if (imported) throw new InvalidDataException("An inventory must be paired with exactly one RPF.");
            var rows = ReadInventory(source, inventory).ToArray();
            catalog.Assets.AddRange(rows);
            offsets = rows.Where(row => row.Evidence.StartsWith("zlib", StringComparison.Ordinal))
                .Select(row => row.Range!.Offset).ToArray();
            imported = true;
        }
        var drawables = ResourceModels.Available ? ResourceModels.List(source, report, cancellationToken) : null;
        ScanRpf(catalog, source, input, offsets ?? drawables?.CandidateOffsets, report, cancellationToken);
        if (drawables is not null) ResourceModels.AddDrawables(catalog, source, firstAsset, drawables.Rows);
        if (cache) SourceCatalogCache.Store(catalog, source, firstAsset);
    }

    public static void ConvertCarveIndex(string index, string output)
    {
        using var data = JsonDocument.Parse(File.ReadAllBytes(index));
        LocalFiles.Write(output, stream =>
        {
            using var writer = new StreamWriter(stream, leaveOpen: true);
            writer.WriteLine("# entry\tordinal\theader\toffset\tstored_end\tstored_len\tdecoded_len\tchunks\tevidence\tsignature");
            int ordinal = 0;
            foreach (var row in data.RootElement.GetProperty("files").EnumerateArray())
            {
                long offset = row.GetProperty("off").GetInt64();
                long length = row.GetProperty("stored").GetInt64();
                string evidence = row.TryGetProperty("type", out _) ? "bink_header" : "zlib_stream";
                writer.WriteLine(FormattableString.Invariant(
                    $"entry\t{ordinal++}\t\t0x{offset:X}\t0x{checked(offset + length):X}\t{length}\t{row.GetProperty("size").GetInt64()}\t1\t{evidence}\t{row.GetProperty("head").GetString()}"));
            }
        });
    }

    private static IEnumerable<Asset> ReadInventory(Source source, string path)
    {
        foreach (string line in File.ReadLines(path))
        {
            string[] fields = line.Split('\t');
            if (fields[0] != "entry") continue;
            if (fields.Length < 10) throw new InvalidDataException("Incomplete RPF inventory entry.");
            var range = new ByteRange(Catalog.Number(fields[3]), Catalog.Number(fields[5]), Catalog.Number(fields[6]));
            range.Validate(source.Fingerprint.Length);
            if (range.Offset + range.Length != Catalog.Number(fields[4]))
                throw new InvalidDataException("Stored length and end disagree in inventory.");
            string evidence = fields[8];
            yield return new($"{source.Id}:range:{range.Offset:X}", source.Id, "unknown", $"0x{range.Offset:X}",
                "alias", evidence, Range: range);
        }
    }

    private static void ScanRpf(Catalog catalog, Source source, FileStream file, IReadOnlyList<long>? offsets,
        Action<IndexProgress>? report, CancellationToken cancellationToken)
    {
        if (offsets is null) report?.Invoke(new("Finding model candidates", 0, 0, "entries"));
        var models = offsets ?? Rpf9ModelRecovery.FindCandidateOffsets(source.Path, cancellationToken: cancellationToken);
        int completed = 0;
        foreach (var batch in models.Chunk(32))
        {
            report?.Invoke(new("Validating model candidates", completed, models.Count, "entries"));
            completed += batch.Length;
            foreach (var entry in Rpf9ModelRecovery.RecoverAtOffsets(source.Path, file.Length, batch, cancellationToken: cancellationToken))
            {
                var asset = Locator(source, entry, "model");
                var raw = DecodeSegments(file, asset);
                var model = RageModelDecoder.Decode(raw.AsSpan(0, asset.SystemBytes!.Value), raw.AsSpan(asset.SystemBytes.Value));
                if (model is null) continue;
                if (!string.IsNullOrWhiteSpace(model.Name)) asset = asset with { Name = model.Name, NameOrigin = "resource" };
                catalog.Assets.Add(asset);
                foreach (var t in model.EmbeddedTextures ?? [])
                    catalog.Assets.Add(TextureChild(source, asset, t, "embedded dictionary"));
            }
        }
        report?.Invoke(new("Validating model candidates", completed, models.Count, "entries"));
        if (offsets is null) report?.Invoke(new("Finding texture candidates", 0, 0, "entries"));
        var textures = offsets ?? Rpf9TextureRecovery.FindCandidateOffsets(source.Path, cancellationToken: cancellationToken);
        completed = 0;
        foreach (var batch in textures.Chunk(32))
        {
            report?.Invoke(new("Validating texture candidates", completed, textures.Count, "entries"));
            completed += batch.Length;
            foreach (var entry in Rpf9TextureRecovery.RecoverAtOffsets(source.Path, file.Length, batch, cancellationToken: cancellationToken))
            {
                var asset = Locator(source, entry, "dictionary");
                var raw = DecodeSegments(file, asset);
                var dictionary = RageTextureDecoder.Inspect(raw.AsSpan(0, asset.SystemBytes!.Value), raw.AsSpan(asset.SystemBytes.Value));
                if (dictionary is null) continue;
                catalog.Assets.Add(asset);
                foreach (var t in dictionary.Textures)
                    catalog.Assets.Add(TextureChild(source, asset, t, "RPF texture dictionary"));
            }
        }
        report?.Invoke(new("Validating texture candidates", completed, textures.Count, "entries"));
    }

    private static Asset Locator(Source source, ArchiveEntry entry, string type)
    {
        var segments = entry.Segments!.Select(segment => new ByteRange(segment.Offset, segment.StoredSize, segment.Size)).ToArray();
        var last = segments[^1];
        return new($"{source.Id}:{type}:{entry.Offset:X}", source.Id, type, entry.Name, "alias",
            entry.Compression, new(entry.Offset, checked(last.Offset + last.Length - entry.Offset), entry.Size),
            segments, entry.ResourceSystemBytes);
    }

    private static Asset TextureChild(Source source, Asset parent, RageTextureInfo texture, string evidence) =>
        new($"{parent.Id}:texture:{texture.Index}", source.Id, "texture", texture.Name,
            texture.Name.StartsWith("texture_", StringComparison.OrdinalIgnoreCase) ? "alias" : "resource",
            evidence, Index: texture.Index, NativeKey: Key(texture.AssetKey),
            NameHash: Key(texture.NameHash), ParentId: parent.Id);

    internal static string? Key(ulong? key) => key is null or 0 ? null : key.Value.ToString("X16");

    internal static byte[] DecodeSegments(FileStream file, Asset asset)
    {
        if (asset.Segments is not { Length: > 0 } segments || asset.SystemBytes is not > 0)
            throw new InvalidDataException("A validated segmented resource is required.");
        long size = segments.Sum(segment => segment.DecodedLength!.Value);
        if (size > MaxResourceBytes || asset.SystemBytes >= size)
            throw new InvalidDataException("Invalid resource section lengths.");
        byte[] output = new byte[(int)size];
        int at = 0;
        foreach (var segment in segments)
        {
            segment.Validate(file.Length);
            if (segment.Length > MaxResourceBytes) throw new InvalidDataException("Compressed segment exceeds the resource limit.");
            byte[] packed = new byte[(int)segment.Length];
            file.Position = segment.Offset;
            file.ReadExactly(packed);
            int count = checked((int)segment.DecodedLength!.Value);
            using var encoded = new MemoryStream(packed, writable: false);
            using var decoded = new ZLibStream(encoded, CompressionMode.Decompress);
            decoded.ReadExactly(output.AsSpan(at, count));
            if (decoded.ReadByte() != -1) throw new InvalidDataException("Decoded segment exceeds its catalog length.");
            at += count;
        }
        return output;
    }

    public static RageModel ReadModel(Catalog catalog, Asset asset)
    {
        if (asset.Type != "model") throw new InvalidDataException("Select a validated model.");
        var source = catalog.SourceOf(asset);
        using var file = source.Open();
        source.Fingerprint.Check(source.Path, file, false);
        if (asset.DrawableLocator is not null) return ResourceModels.Read(source, asset);
        var data = DecodeSegments(file, asset);
        return RageModelDecoder.Decode(data.AsSpan(0, asset.SystemBytes!.Value), data.AsSpan(asset.SystemBytes.Value))
            ?? throw new InvalidDataException("Model structure failed validation.");
    }

    public static (byte[] Dds, TextureExport Info) ReadTexture(Catalog catalog, Asset asset)
    {
        if (asset.Type != "texture" || asset.Index is not int index)
            throw new InvalidDataException("Select a validated texture.");
        var source = catalog.SourceOf(asset);
        using var file = source.Open();
        source.Fingerprint.Check(source.Path, file, false);
        byte[] dds;
        int width, height, sourceMips, sourceFormat, baseMip = 0;
        string decoder;
        if (source.Format == "ttpf")
        {
            var t = TtpfReader.Inspect(source.Path).Textures.Single(item => item.Index == index);
            if (Key(t.AssetKey) != asset.NativeKey) throw new InvalidDataException("Native texture key differs.");
            dds = TtpfReader.CreateDds(source.Path, index);
            (width, height, sourceFormat, sourceMips, baseMip) = (t.Width, t.Height, t.DxgiFormat, t.SourceMips, t.BaseMipLevel);
            decoder = nameof(TtpfReader);
        }
        else
        {
            var parent = catalog.Get(asset.ParentId ?? throw new InvalidDataException("Missing RPF texture container."));
            var data = DecodeSegments(file, parent);
            var system = data.AsSpan(0, parent.SystemBytes!.Value);
            var graphics = data.AsSpan(parent.SystemBytes.Value);
            int? dictionaryOffset = null;
            if (parent.Type == "model")
                dictionaryOffset = (RageModelDecoder.Decode(system, graphics)?.EmbeddedTextureDictionaryOffset)
                    ?? throw new InvalidDataException("Embedded texture dictionary is unavailable.");
            var dict = dictionaryOffset is int start
                ? RageTextureDecoder.InspectEmbeddedDictionary(system, graphics.Length, start)
                : RageTextureDecoder.Inspect(system, graphics);
            var t = dict?.Textures.SingleOrDefault(item => item.Index == index)
                ?? throw new InvalidDataException("Texture structure failed validation.");
            if (Key(t.AssetKey) != asset.NativeKey || Key(t.NameHash) != asset.NameHash)
                throw new InvalidDataException("RPF texture identifiers differ.");
            if (dictionaryOffset is int at)
            {
                baseMip = t.PreviewMipLevel;
                dds = CreatePreviewDds(RageTextureDecoder.DecodeEmbeddedTexturePixels(system, graphics, at, index, baseMip));
            }
            else dds = RageTextureDecoder.CreateDds(system, graphics, index);
            (width, height, sourceFormat, sourceMips) = (t.Width, t.Height, t.DxgiFormat, t.MipCount);
            decoder = nameof(RageTextureDecoder);
        }
        var info = DdsTextureReader.Inspect(dds);
        string coverage = baseMip == 0 && info.Mips == sourceMips ? "complete mip chain" : "partial mip chain";
        if (sourceFormat != info.DxgiFormat) coverage += ", converted pixels";
        return (dds, new(info.Width, info.Height, info.DxgiFormat, width, height, sourceFormat, sourceMips, info.Mips,
            baseMip, decoder, coverage));
    }

    public static byte[] CreatePreviewDds(RageTexturePixels pixels)
    {
        using var output = new MemoryStream();
        using var writer = new BinaryWriter(output);
        writer.Write("DDS "u8);
        foreach (int field in new[] { 124, 0x100F, pixels.Height, pixels.Width, pixels.Width * 4, 0, 1 })
            writer.Write(field);
        writer.Write(new byte[44]);
        foreach (int field in new[] { 32, 4, 0x30315844, 0, 0, 0, 0, 0, 0x1000, 0, 0, 0, 0, 87, 3, 0, 1, 0 })
            writer.Write(field);
        writer.Write(pixels.Bgra32);
        return output.ToArray();
    }

    public static object Export(Catalog catalog, Asset asset, string output)
    {
        if (asset.Type == "texture")
        {
            var (bytes, info) = ReadTexture(catalog, asset);
            LocalFiles.Write(output, stream => stream.Write(bytes));
            return info;
        }
        var model = ReadModel(catalog, asset);
        string obj = RageModelDecoder.CreateObj(model);
        LocalFiles.Write(output, stream =>
        {
            using var writer = new StreamWriter(stream, leaveOpen: true);
            writer.Write(obj);
        });
        return new MeshExport(model.Meshes.Sum(mesh => mesh.Positions.Count), model.Meshes.Sum(mesh => mesh.Indices.Count) / 3,
            model.Meshes.Count, model.Materials?.Count ?? 0,
            asset.DrawableLocator is null ? nameof(RageModelDecoder) : "Resource Tools", "static geometry and available UVs");
    }
}
