using RpfPreview.Core;

namespace ViceModVI;

public static class PoolWriter
{
    public static object Copy(Catalog catalog, Asset asset, byte[] dds, string output)
    {
        var source = catalog.SourceOf(asset);
        if (source.Format != "ttpf" || asset.Type != "texture" || asset.Index is not int index)
            throw new NotSupportedException("The copy writer requires a TTPF texture.");
        output = Path.GetFullPath(output);
        if (File.Exists(output) || Directory.Exists(output)) throw new IOException("Choose a new destination.");
        using var input = source.Open();
        source.Fingerprint.Check(source.Path, input, true);
        var pool = TtpfReader.Inspect(source.Path);
        var selected = pool.Textures.Single(texture => texture.Index == index);
        if (XboxAssets.Key(selected.AssetKey) != asset.NativeKey)
            throw new InvalidDataException("Native texture key differs.");
        if (!selected.SupportsNativeReplacement)
            throw new NotSupportedException("This texture storage profile cannot be written.");
        var neighbor = pool.Textures.Where(texture => texture.Index != index && texture.CanExtract)
            .OrderBy(texture => Math.Abs(texture.Index - index)).FirstOrDefault()
            ?? throw new NotSupportedException("A readable neighboring entry is required to verify the copy.");
        byte[] original = TtpfReader.CreateDds(source.Path, index);
        byte[] adjacent = TtpfReader.CreateDds(source.Path, neighbor.Index);
        string temporary = Path.Combine(Path.GetDirectoryName(output)!, $".vicemodvi-{Guid.NewGuid():N}.ttpf");
        try
        {
            var result = Rds5TextureWriter.ExportReplacementCopy(source.Path, index, dds, temporary);
            var reopened = TtpfReader.Inspect(temporary);
            if (reopened.Textures.Count != pool.Textures.Count ||
                reopened.Textures.Single(texture => texture.Index == index).AssetKey != selected.AssetKey)
                throw new InvalidDataException("The copy's texture index changed.");
            byte[] actual = TtpfReader.CreateDds(temporary, index);
            if (!Payload(actual).SequenceEqual(Payload(dds)) ||
                !TtpfReader.CreateDds(temporary, neighbor.Index).AsSpan().SequenceEqual(adjacent))
                throw new InvalidDataException("Reopened pool failed replacement or neighbor verification.");
            source.Fingerprint.Check(source.Path, input, true);
            File.Move(temporary, output);
            return new
            {
                Index = index, asset.NativeKey, Neighbor = neighbor.Index, NeighborVerified = true,
                ContentChanged = !Payload(actual).SequenceEqual(Payload(original)),
                result.ChangedChunks, result.ChangedDecodedBytes, result.SourceBytes, result.DestinationBytes,
                Compatibility = "Pending platform and build verification"
            };
        }
        finally { File.Delete(temporary); }
    }

    private static ReadOnlySpan<byte> Payload(byte[] dds)
    {
        var info = DdsTextureReader.Inspect(dds);
        return dds.AsSpan(info.DataOffset, info.DataSize);
    }
}
