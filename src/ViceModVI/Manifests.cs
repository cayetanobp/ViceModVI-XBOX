using System.Text.Json;

namespace ViceModVI;

public static class Manifests
{
    public static object Import(Catalog catalog, string sourceId, string path)
    {
        var source = catalog.Sources.Single(item => item.Id == sourceId);
        using var file = source.Open();
        source.Fingerprint.Check(source.Path, file, false);
        using var document = JsonDocument.Parse(File.ReadAllBytes(path));
        var root = document.RootElement;
        if (root.TryGetProperty("sha256", out var digest) &&
            !string.Equals(digest.GetString(), source.Fingerprint.Sha256, StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("Manifest SHA-256 differs from the catalog source.");
        var rows = source.Format == "ttpf" ? root.GetProperty("Textures") : root.GetProperty("Items");
        var assets = catalog.Assets.Select((asset, index) => (asset, index))
            .Where(item => item.asset.SourceId == sourceId).ToArray();
        int matched = 0, unresolved = 0, differentNames = 0;
        void Match(JsonElement row, IEnumerable<(Asset asset, int index)> candidates, string type)
        {
            string? key = Text(row, "AssetKey") ?? Text(row, "Key");
            if (key is not null)
            {
                string hex = Catalog.HexKey(key);
                candidates = candidates.Where(item => item.asset.NativeKey == hex ||
                    (hex == "0000000000000000" && item.asset.NativeKey is null));
            }
            var matches = candidates.Take(2).ToArray();
            if (matches.Length != 1)
            {
                unresolved++;
                return;
            }
            var (asset, index) = matches[0];
            var current = catalog.Assets[index];
            string? name = Text(row, "Name");
            if (name is not null && current.NameOrigin == "resource" && current.Name != name) differentNames++;
            catalog.Assets[index] = current.NameOrigin == "alias" && !string.IsNullOrWhiteSpace(name) && name != current.Name
                ? current with { Name = name, NameOrigin = "manifest", ManifestType = type }
                : current with { ManifestType = type };
            matched++;
        }
        foreach (var row in rows.EnumerateArray())
        {
            if (source.Format == "ttpf")
            {
                int index = row.GetProperty("Index").GetInt32();
                Match(row, assets.Where(item => item.asset.Index == index), "texture");
                continue;
            }
            long offset = row.GetProperty("Offset").GetInt64();
            bool dictionary = row.TryGetProperty("Textures", out var children);
            string type = dictionary ? "dictionary" : "model";
            var parents = assets.Where(item => item.asset.Type == type && item.asset.Range?.Offset == offset).ToArray();
            Match(row, parents, type);
            if (dictionary || row.TryGetProperty("Embedded", out children))
            {
                if (children.ValueKind != JsonValueKind.Array || parents.Length != 1) continue;
                foreach (var child in children.EnumerateArray())
                {
                    int index = child.GetProperty("Index").GetInt32();
                    Match(child, assets.Where(item => item.asset.ParentId == parents[0].asset.Id && item.asset.Index == index), "texture");
                }
            }
        }
        return new { matched, unresolved, differentNames };
    }

    private static string? Text(JsonElement row, string key) =>
        row.TryGetProperty(key, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() : null;
}
