using System.Globalization;
using System.Security.Cryptography;
using System.Text.Json;

namespace ViceModVI;

public sealed record ByteRange(long Offset, long Length, long? DecodedLength = null)
{
    public void Validate(long fileLength)
    {
        if (Offset < 0 || Length <= 0 || Offset > fileLength || Length > fileLength - Offset)
            throw new InvalidDataException($"Range 0x{Offset:X}+{Length:N0} is outside the source ({fileLength:N0} bytes).");
    }
}

public sealed record Fingerprint(long Length, string Sha256, string SamplesSha256, DateTime LastWriteUtc)
{
    public static Fingerprint Create(string path, FileStream file, Action<long>? progress = null,
        CancellationToken cancellationToken = default)
    {
        var stamp = File.GetLastWriteTimeUtc(path);
        file.Position = 0;
        string sha;
        if (progress is null && !cancellationToken.CanBeCanceled) sha = Convert.ToHexString(SHA256.HashData(file));
        else
        {
            using var digest = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
            byte[] buffer = new byte[1024 * 1024];
            long nextReport = 0;
            int count;
            while ((count = file.Read(buffer)) != 0)
            {
                cancellationToken.ThrowIfCancellationRequested();
                digest.AppendData(buffer.AsSpan(0, count));
                if (file.Position < nextReport) continue;
                progress?.Invoke(file.Position);
                nextReport = file.Position + 32L * 1024 * 1024;
            }
            progress?.Invoke(file.Position);
            sha = Convert.ToHexString(digest.GetHashAndReset());
        }
        var sample = Sample(file);
        if (stamp != File.GetLastWriteTimeUtc(path))
            throw new IOException("Source changed while being fingerprinted.");
        return new(file.Length, sha, sample.Hash, stamp);
    }

    public long Check(string path, FileStream file, bool full)
    {
        if (file.Length != Length || (!full && File.GetLastWriteTimeUtc(path) != LastWriteUtc))
            throw new InvalidDataException($"Source fingerprint differs: {path}");
        if (full)
        {
            file.Position = 0;
            if (Convert.ToHexString(SHA256.HashData(file)) != Sha256)
                throw new InvalidDataException($"Source SHA-256 differs: {path}");
            return file.Length;
        }
        var sample = Sample(file);
        if (sample.Hash != SamplesSha256 || File.GetLastWriteTimeUtc(path) != LastWriteUtc)
            throw new InvalidDataException($"Source fingerprint differs: {path}");
        return sample.Bytes;
    }

    private static (string Hash, long Bytes) Sample(FileStream file)
    {
        const int count = 8;
        const int block = 4096;
        using var hash = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
        var buffer = new byte[block];
        long length = file.Length;
        if (length <= count * block)
        {
            file.Position = 0;
            return (Convert.ToHexString(SHA256.HashData(file)), length);
        }
        for (int i = 0; i < count; i++)
        {
            long span = length - block;
            file.Position = span / (count - 1) * i + span % (count - 1) * i / (count - 1);
            file.ReadExactly(buffer);
            hash.AppendData(buffer);
        }
        return (Convert.ToHexString(hash.GetHashAndReset()), count * block);
    }
}

public sealed record Source(string Id, string Path, string Format, Fingerprint Fingerprint)
{
    public FileStream Open() => new(Path, FileMode.Open, FileAccess.Read, FileShare.Read, 1);
}

public sealed record Asset(string Id, string SourceId, string Type, string Name, string NameOrigin,
    string Evidence, ByteRange? Range = null, ByteRange[]? Segments = null, int? SystemBytes = null,
    int? Index = null, string? NativeKey = null, string? NameHash = null, string? ParentId = null,
    string? ManifestType = null, string? DrawableLocator = null);

public sealed record ReadMeasurement(string AssetId, long VerificationBytes, long PayloadBytes)
{
    public long TotalBytes => checked(VerificationBytes + PayloadBytes);
}

public sealed class Catalog
{
    public const int CurrentVersion = 1;
    public int Version { get; init; } = CurrentVersion;
    public List<Source> Sources { get; init; } = [];
    public List<Asset> Assets { get; init; } = [];

    public Source SourceOf(Asset asset) => Sources.Single(source => source.Id == asset.SourceId);

    public IEnumerable<Asset> Find(string? source = null, string? type = null, string? name = null,
        string? hash = null, long? offset = null)
    {
        var sources = Sources.Where(item => source is null || item.Id == source ||
            item.Path.Contains(source, StringComparison.OrdinalIgnoreCase)).Select(item => item.Id).ToHashSet();
        string? key = hash is null ? null : HexKey(hash);
        return Assets.Where(asset => sources.Contains(asset.SourceId) &&
            (type is null || string.Equals(asset.Type, type, StringComparison.OrdinalIgnoreCase)) &&
            (name is null || asset.Name.Contains(name, StringComparison.OrdinalIgnoreCase)) &&
            (key is null || asset.NativeKey == key || asset.NameHash == key) &&
            (offset is null || asset.Range?.Offset == offset));
    }

    public Asset Get(string id) => Assets.SingleOrDefault(asset => asset.Id == id)
        ?? throw new InvalidDataException($"Catalog entry not found: {id}");

    public static string HexKey(string value)
    {
        var text = value.StartsWith("0x", StringComparison.OrdinalIgnoreCase) ? value[2..] : value;
        if (!ulong.TryParse(text, NumberStyles.AllowHexSpecifier, CultureInfo.InvariantCulture, out ulong key))
            throw new InvalidDataException($"Invalid hexadecimal identifier: {value}");
        return key.ToString("X16");
    }

    public static long Number(string value) => value.StartsWith("0x", StringComparison.OrdinalIgnoreCase)
        ? long.Parse(value.AsSpan(2), NumberStyles.AllowHexSpecifier, CultureInfo.InvariantCulture)
        : long.Parse(value, NumberStyles.Integer, CultureInfo.InvariantCulture);

    public static Catalog Load(string file)
    {
        var catalog = LocalFiles.Load<Catalog>(file);
        catalog.Validate();
        return catalog;
    }

    public void Save(string file, bool replace = false)
    {
        if (Sources.Any(source => string.Equals(source.Path, Path.GetFullPath(file), StringComparison.OrdinalIgnoreCase)))
            throw new IOException("A catalog cannot replace a source file.");
        Validate();
        LocalFiles.Save(file, this, replace);
    }

    internal void Validate()
    {
        if (Version != CurrentVersion) throw new InvalidDataException($"Unsupported catalog version {Version}.");
        var sources = Sources.ToDictionary(source => source.Id, StringComparer.Ordinal);
        var assets = Assets.ToDictionary(asset => asset.Id, StringComparer.Ordinal);
        foreach (var asset in Assets)
        {
            if (!sources.TryGetValue(asset.SourceId, out var source))
                throw new InvalidDataException($"Unknown source for {asset.Id}.");
            asset.Range?.Validate(source.Fingerprint.Length);
            if (asset.Index < 0)
                throw new InvalidDataException($"Invalid local index for {asset.Id}.");
            if (asset.ParentId is { } parentId && (!assets.TryGetValue(parentId, out var parent) ||
                parent.SourceId != asset.SourceId || parent.Type is not ("model" or "dictionary")))
                throw new InvalidDataException($"Invalid parent for {asset.Id}.");
            if (asset.Segments is not { Length: > 0 } segments) continue;
            if (asset.Range is not { } extent || extent.Offset != segments[0].Offset)
                throw new InvalidDataException($"Missing segment extent for {asset.Id}.");
            long end = extent.Offset;
            foreach (var segment in segments)
            {
                segment.Validate(source.Fingerprint.Length);
                if (segment.Offset < end || segment.Offset + segment.Length > extent.Offset + extent.Length
                    || segment.DecodedLength is null or <= 0)
                    throw new InvalidDataException($"Invalid segment for {asset.Id}.");
                end = segment.Offset + segment.Length;
            }
        }
    }

    public ReadMeasurement CopyStored(Asset asset, Stream output, string? alternateSource = null)
    {
        var source = SourceOf(asset);
        string path = alternateSource is null ? source.Path : Path.GetFullPath(alternateSource);
        using var file = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read, 1);
        long checkedBytes = source.Fingerprint.Check(path, file, full: alternateSource is not null);
        var range = asset.Range ?? throw new InvalidDataException("This entry is addressed by its native index.");
        range.Validate(file.Length);
        file.Position = range.Offset;
        byte[] buffer = new byte[65536];
        long remaining = range.Length;
        while (remaining > 0)
        {
            int count = (int)Math.Min(remaining, buffer.Length);
            file.ReadExactly(buffer.AsSpan(0, count));
            output.Write(buffer, 0, count);
            remaining -= count;
        }
        return new(asset.Id, checkedBytes, range.Length);
    }
}

public static class LocalFiles
{
    public static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web) { WriteIndented = true };

    public static T Load<T>(string path)
    {
        using var stream = File.OpenRead(path);
        return JsonSerializer.Deserialize<T>(stream, Json) ?? throw new InvalidDataException("Empty JSON document.");
    }

    public static void Save<T>(string path, T value, bool replace = false) =>
        Write(path, stream => JsonSerializer.Serialize(stream, value, Json), replace);

    public static void Write(string path, Action<Stream> write, bool replace = false)
    {
        path = Path.GetFullPath(path);
        string temporary = Path.Combine(Path.GetDirectoryName(path)!, $".vicemodvi-{Guid.NewGuid():N}.tmp");
        try
        {
            using (var stream = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None))
            {
                write(stream);
                stream.Flush(true);
            }
            File.Move(temporary, path, replace);
        }
        finally { File.Delete(temporary); }
    }
}
