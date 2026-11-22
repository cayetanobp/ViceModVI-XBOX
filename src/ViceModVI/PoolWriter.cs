using RpfPreview.Core;

namespace ViceModVI;

public sealed record PoolWriteResult(int Index, string? NativeKey, int Neighbor, bool NeighborVerified,
    bool ReplacedSource, bool ContentChanged, int ChangedChunks, long ChangedDecodedBytes,
    long SourceBytes, long DestinationBytes, string Compatibility, TextureConversionPlan Conversion);

public static class PoolWriter
{
    public static TextureConversionPlan Plan(Catalog catalog, Asset asset, byte[] dds)
    {
        var (source, index) = Locate(catalog, asset);
        using var input = source.Open();
        source.Fingerprint.Check(source.Path, input, false);
        var texture = FindTexture(asset, TtpfReader.Inspect(source.Path), index);
        return Plan(texture, dds);
    }

    private static TextureConversionPlan Plan(TtpfTextureInfo texture, byte[] dds)
    {
        string? limitation = texture.Limitation;
        if (!texture.SupportsNativeReplacement && limitation is null)
            limitation = texture.PartialMips
                ? $"RDS5 entry {texture.Index} exposes {texture.Mips} of {texture.SourceMips} mips starting at mip {texture.BaseMipLevel}. " +
                  "The writer needs a readable full native mip chain."
                : $"RDS5 entry {texture.Index} uses a storage layout that the current writer supports only for reading.";
        if (texture.Dimension != 1 || texture.Depth != 1)
            limitation ??= "The current RDS5 writer requires a single 2D texture descriptor.";
        return TextureConversion.Plan(dds, new(texture.Width, texture.Height, texture.DxgiFormat, texture.SourceMips), limitation);
    }

    private static (Source Source, int Index) Locate(Catalog catalog, Asset asset)
    {
        var source = catalog.SourceOf(asset);
        if (source.Format != "ttpf" || asset.Type != "texture" || asset.Index is not int index)
            throw new NotSupportedException("Select a TTPF texture for this export.");
        return (source, index);
    }

    private static TtpfTextureInfo FindTexture(Asset asset, TtpfInfo pool, int index)
    {
        var texture = pool.Textures.Single(item => item.Index == index);
        if (XboxAssets.Key(texture.AssetKey) != asset.NativeKey)
            throw new InvalidDataException("Native texture key differs.");
        return texture;
    }

    public static PoolWriteResult Copy(Catalog catalog, Asset asset, byte[] dds, string output,
        bool overwriteExisting = false, bool allowResize = false)
    {
        var (source, index) = Locate(catalog, asset);
        output = Path.GetFullPath(output);
        if (Directory.Exists(output)) throw new IOException("The destination is a directory.");
        if (File.Exists(output) && !overwriteExisting)
            throw new IOException("The destination already exists. Use --overwrite or choose a new path.");
        using (var input = source.Open()) source.Fingerprint.Check(source.Path, input, true);
        var pool = TtpfReader.Inspect(source.Path);
        var selected = FindTexture(asset, pool, index);
        var conversion = Plan(selected, dds);
        byte[] prepared = TextureConversion.Apply(dds, conversion, allowResize);
        var neighbor = pool.Textures.Where(texture => texture.Index != index && texture.CanExtract)
            .OrderBy(texture => Math.Abs(texture.Index - index)).FirstOrDefault()
            ?? throw new NotSupportedException("A readable neighboring entry is required to verify the copy.");
        byte[] original = TtpfReader.CreateDds(source.Path, index);
        byte[] adjacent = TtpfReader.CreateDds(source.Path, neighbor.Index);
        string temporary = Path.Combine(Path.GetDirectoryName(output)!, $".vicemodvi-{Guid.NewGuid():N}.ttpf");
        try
        {
            var result = Rds5TextureWriter.ExportReplacementCopy(source.Path, index, prepared, temporary);
            var reopened = TtpfReader.Inspect(temporary);
            if (reopened.Textures.Count != pool.Textures.Count ||
                reopened.Textures.Single(texture => texture.Index == index).AssetKey != selected.AssetKey)
                throw new InvalidDataException("The copy's texture index changed.");
            byte[] actual = TtpfReader.CreateDds(temporary, index);
            if (!Payload(actual).SequenceEqual(Payload(prepared)) ||
                !TtpfReader.CreateDds(temporary, neighbor.Index).AsSpan().SequenceEqual(adjacent))
                throw new InvalidDataException("Reopened pool failed replacement or neighbor verification.");
            using (var current = source.Open()) source.Fingerprint.Check(source.Path, current, true);
            File.Move(temporary, output, overwriteExisting);
            bool replacedSource = string.Equals(output, Path.GetFullPath(source.Path), StringComparison.OrdinalIgnoreCase);
            if (replacedSource)
            {
                using var current = source.Open();
                var refreshed = Fingerprint.Create(source.Path, current);
                int sourceIndex = catalog.Sources.FindIndex(item => item.Id == source.Id);
                catalog.Sources[sourceIndex] = source with { Fingerprint = refreshed };
            }
            return new(index, asset.NativeKey, neighbor.Index, true, replacedSource,
                !Payload(actual).SequenceEqual(Payload(original)), result.ChangedChunks,
                result.ChangedDecodedBytes, result.SourceBytes, result.DestinationBytes,
                "Pending platform and build verification", conversion);
        }
        finally { File.Delete(temporary); }
    }

    private static ReadOnlySpan<byte> Payload(byte[] dds)
    {
        var info = DdsTextureReader.Inspect(dds);
        return dds.AsSpan(info.DataOffset, info.DataSize);
    }
}
