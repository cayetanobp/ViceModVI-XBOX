using RpfPreview.Core;

namespace ViceModVI;

public sealed record ReplacementTarget(string AssetId, string SourceSha256, int? Index, string? NativeKey, string? ParentId);
public sealed record StagedTexture(ReplacementTarget Target, string OriginalFileName, byte[] Dds);
public sealed record TextureDifference(string Field, string Original, string Replacement);

public sealed class ReplacementProject
{
    public int Version { get; init; } = 1;
    public required string CatalogPath { get; init; }
    public List<StagedTexture> Replacements { get; init; } = [];
    public string? SelectedAssetId { get; set; }
    public int? SelectedMesh { get; set; }
    public bool ShowReplacement { get; set; }

    public void Save(string path, Catalog catalog)
    {
        string destination = Path.GetFullPath(path);
        if (string.Equals(destination, CatalogPath, StringComparison.OrdinalIgnoreCase) ||
            catalog.Sources.Any(source => string.Equals(source.Path, destination, StringComparison.OrdinalIgnoreCase)))
            throw new IOException("Choose a separate .vmp file for the replacement project.");
        LocalFiles.Save(destination, this, replace: true);
    }

    public static ReplacementProject Load(string path, out Catalog catalog)
    {
        var project = LocalFiles.Load<ReplacementProject>(path);
        if (project.Version != 1) throw new InvalidDataException("Unsupported replacement project version.");
        catalog = Catalog.Load(project.CatalogPath);
        var verified = new HashSet<string>();
        var seen = new HashSet<string>();
        foreach (var replacement in project.Replacements)
        {
            var asset = catalog.Get(replacement.Target.AssetId);
            if (asset.Type != "texture" || !seen.Add(asset.Id) || replacement.Target != Identify(catalog, asset))
                throw new InvalidDataException("A staged replacement targets a different catalog entry.");
            var source = catalog.SourceOf(asset);
            if (verified.Add(source.Id))
            {
                using var file = source.Open();
                source.Fingerprint.Check(source.Path, file, full: true);
            }
            DdsTextureReader.Inspect(replacement.Dds);
        }
        return project;
    }

    public void Stage(Catalog catalog, Asset asset, string name, byte[] dds)
    {
        if (asset.Type != "texture") throw new InvalidDataException("Select a texture to replace.");
        var replacement = DdsTextureReader.Inspect(dds);
        if (replacement.SliceCount != 1 || replacement.Dimension != 1 || replacement.Depth != 1)
            throw new NotSupportedException("The local viewer accepts a single 2D replacement image.");
        DdsTextureReader.DecodeTexturePixels(dds);
        var staged = new StagedTexture(Identify(catalog, asset), Path.GetFileName(name), dds);
        int index = Replacements.FindIndex(item => item.Target.AssetId == asset.Id);
        if (index >= 0) Replacements[index] = staged;
        else Replacements.Add(staged);
    }

    public StagedTexture? Find(Asset asset) => Replacements.SingleOrDefault(item => item.Target.AssetId == asset.Id);

    public bool Revert(Asset asset) => Replacements.RemoveAll(item => item.Target.AssetId == asset.Id) != 0;

    public static TextureDifference[] Compare(TextureExport original, DdsTextureInfo replacement)
    {
        TextureDifference[] fields =
        [
            new("Dimensions", $"{original.SourceWidth} × {original.SourceHeight}", $"{replacement.Width} × {replacement.Height}"),
            new("DXGI format", original.SourceFormat.ToString(), replacement.DxgiFormat.ToString()),
            new("Mip levels", original.SourceMips.ToString(), replacement.Mips.ToString())
        ];
        return fields.Where(value => value.Original != value.Replacement).ToArray();
    }

    private static ReplacementTarget Identify(Catalog catalog, Asset asset) =>
        new(asset.Id, catalog.SourceOf(asset).Fingerprint.Sha256, asset.Index, asset.NativeKey, asset.ParentId);
}
