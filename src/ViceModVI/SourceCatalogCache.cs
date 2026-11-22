using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace ViceModVI;

internal static class SourceCatalogCache
{
    private const string Revision = "2";
    private sealed record Snapshot(Source Source, Asset[] Assets);

    private static string CachePath(string path)
    {
        string variant = ResourceModels.Available ? "rust" : "legacy";
        string identity = OperatingSystem.IsWindows() ? path.ToUpperInvariant() : path;
        string key = Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(identity)));
        return Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "ViceModVI", "source-index-" + Revision + "-" + variant, key + ".json");
    }

    public static bool TryRestore(Catalog catalog, string path)
    {
        try
        {
            string cache = CachePath(path);
            if (!File.Exists(cache)) return false;
            var cached = LocalFiles.Load<Snapshot>(cache);
            var file = new FileInfo(path);
            if (cached.Source.Path != path || cached.Source.Fingerprint.Length != file.Length ||
                cached.Source.Fingerprint.LastWriteUtc != file.LastWriteTimeUtc) return false;
            using (var stream = cached.Source.Open()) cached.Source.Fingerprint.Check(path, stream, full: false);
            string id = catalog.Sources.Count.ToString();
            string Remap(string oldId) => id + oldId[cached.Source.Id.Length..];
            var assets = cached.Assets.Select(asset => asset with
            {
                Id = Remap(asset.Id), SourceId = id,
                ParentId = asset.ParentId is null ? null : Remap(asset.ParentId)
            }).ToArray();
            catalog.Sources.Add(cached.Source with { Id = id });
            catalog.Assets.AddRange(assets);
            return true;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or JsonException
            or InvalidDataException or ArgumentException)
        {
            return false;
        }
    }

    public static void Store(Catalog catalog, Source source, int firstAsset)
    {
        try
        {
            string cache = CachePath(source.Path);
            Directory.CreateDirectory(Path.GetDirectoryName(cache)!);
            LocalFiles.Save(cache, new Snapshot(source, catalog.Assets.Skip(firstAsset).ToArray()), replace: true);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            // Opening remains available when the local cache cannot be written.
        }
    }
}
