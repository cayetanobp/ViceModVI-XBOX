using System.IO.Compression;
using System.Security.Cryptography;
using RpfPreview.Core;

namespace ViceModVI;

public sealed record RpfCopyResult(string TextureId, long ArchiveBytes, string OriginalSha256, string CopySha256,
    int RewrittenSegments, int? VerifiedNeighbor, TextureConversionPlan Conversion, string Compatibility);

public static class RpfCopyWriter
{
    private sealed record Patch(long Offset, byte[] Packed);

    public static RpfCopyResult Copy(Catalog catalog, Asset asset, byte[] dds, string destination, bool allowResize = false)
    {
        var source = catalog.SourceOf(asset);
        if (source.Format != "rpf9" || asset.Type != "texture" || asset.Index is not int index ||
            asset.ParentId is not { } parentId)
            throw new NotSupportedException("Select a texture in a recovered RPF9 dictionary.");
        var parent = catalog.Get(parentId);
        if (parent.Type != "dictionary" || parent.SystemBytes is not > 0 || parent.Segments is not { Length: > 0 } segments)
            throw new NotSupportedException("The experimental copy requires a standalone segmented RPF9 dictionary.");
        destination = Path.GetFullPath(destination);
        if (File.Exists(destination) || Directory.Exists(destination))
            throw new IOException("Choose a new file for the RPF9 copy.");

        using var input = source.Open();
        source.Fingerprint.Check(source.Path, input, true);
        byte[] original = XboxAssets.DecodeSegments(input, parent);
        int systemSize = parent.SystemBytes.Value;
        var system = original.AsSpan(0, systemSize);
        var graphics = original.AsSpan(systemSize);
        var dictionary = RageTextureDecoder.Inspect(system, graphics)
            ?? throw new InvalidDataException("The source texture dictionary failed validation.");
        var texture = dictionary.Textures.Single(item => item.Index == index);
        if (XboxAssets.Key(texture.AssetKey) != asset.NativeKey || XboxAssets.Key(texture.NameHash) != asset.NameHash)
            throw new InvalidDataException("Native texture identifiers differ.");
        string? limitation = texture.Dimension != 1 || texture.Depth != 1
            ? "The experimental RPF9 writer supports single 2D textures." : null;
        var plan = TextureConversion.Plan(dds, new(texture.Width, texture.Height, texture.DxgiFormat, texture.MipCount), limitation);
        byte[] expectedDds = TextureConversion.Apply(dds, plan, allowResize);
        var staged = NativeTextureReplacement.Stage(system, graphics, index, expectedDds);
        var patches = PreparePatches(segments, original, staged.ResourceData);
        var neighbor = dictionary.Textures.Where(item => item.Index != index)
            .OrderBy(item => Math.Abs(item.Index - index)).FirstOrDefault();
        byte[]? neighborDds = neighbor is null ? null : RageTextureDecoder.CreateDds(system, graphics, neighbor.Index);

        string scratch = Path.Combine(Path.GetDirectoryName(destination)!, $".vicemodvi-{Guid.NewGuid():N}.rpf");
        try
        {
            using (var output = new FileStream(scratch, FileMode.CreateNew, FileAccess.Write, FileShare.None))
            {
                var buffer = new byte[1024 * 1024];
                input.Position = 0;
                foreach (var patch in patches)
                {
                    CopyBytes(input, output, patch.Offset - input.Position, buffer);
                    output.Write(patch.Packed);
                    input.Position += patch.Packed.Length;
                }
                CopyBytes(input, output, input.Length - input.Position, buffer);
                output.Flush(true);
            }
            string digest;
            using (var reopened = File.OpenRead(scratch))
            {
                digest = CheckCopy(input, reopened, patches);
                byte[] raw = XboxAssets.DecodeSegments(reopened, parent);
                var reopenedSystem = raw.AsSpan(0, systemSize);
                var reopenedGraphics = raw.AsSpan(systemSize);
                byte[] actualDds = RageTextureDecoder.CreateDds(reopenedSystem, reopenedGraphics, index);
                if (!reopenedSystem.SequenceEqual(system) || !raw.AsSpan().SequenceEqual(staged.ResourceData) ||
                    !DdsPayload(actualDds).SequenceEqual(DdsPayload(expectedDds)) ||
                    (neighbor is not null && !RageTextureDecoder.CreateDds(reopenedSystem, reopenedGraphics, neighbor.Index)
                        .AsSpan().SequenceEqual(neighborDds)))
                    throw new InvalidDataException("The reopened RPF9 failed dictionary or texture verification.");
            }
            File.Move(scratch, destination);
            return new(asset.Id, input.Length, source.Fingerprint.Sha256, digest, patches.Count, neighbor?.Index, plan,
                "Pending a test in the exact game build and platform");
        }
        finally { File.Delete(scratch); }
    }

    private static List<Patch> PreparePatches(ByteRange[] segments, byte[] original, byte[] replacement)
    {
        var patches = new List<Patch>();
        int offset = 0;
        foreach (var segment in segments)
        {
            int size = checked((int)segment.DecodedLength!.Value);
            var edited = replacement.AsSpan(offset, size);
            if (!edited.SequenceEqual(original.AsSpan(offset, size)))
            {
                var packed = PackExactly(edited, segment.Length);
                if (packed is null)
                    throw new NotSupportedException($"The changed segment at 0x{segment.Offset:X} could not be encoded in exactly " +
                        $"{segment.Length} bytes. This writer requires the original packed size for each segment.");
                patches.Add(new(segment.Offset, packed));
            }
            offset += size;
        }
        return patches;
    }

    private static byte[]? PackExactly(ReadOnlySpan<byte> decoded, long requiredLength)
    {
        using var output = new MemoryStream();
        foreach (int level in Enumerable.Range(1, 9).Reverse())
        {
            output.SetLength(0);
            using (var stream = new ZLibStream(output, new ZLibCompressionOptions { CompressionLevel = level }, leaveOpen: true))
                stream.Write(decoded);
            if (output.Length == requiredLength) return output.ToArray();
        }
        return null;
    }

    private static void CopyBytes(Stream source, Stream output, long length, byte[] buffer)
    {
        while (length > 0)
        {
            int count = (int)Math.Min(length, buffer.Length);
            source.ReadExactly(buffer.AsSpan(0, count));
            output.Write(buffer, 0, count);
            length -= count;
        }
    }

    private static string CheckCopy(FileStream source, FileStream copy, IReadOnlyList<Patch> patches)
    {
        if (source.Length != copy.Length) throw new InvalidDataException("The copy changed the archive size.");
        using var hash = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
        var expected = new byte[1024 * 1024];
        var actual = new byte[expected.Length];
        source.Position = 0;
        foreach (var patch in patches)
        {
            CompareRange(patch.Offset);
            var found = new byte[patch.Packed.Length];
            copy.ReadExactly(found);
            if (!found.AsSpan().SequenceEqual(patch.Packed))
                throw new InvalidDataException($"Replacement segment differs at 0x{patch.Offset:X}.");
            hash.AppendData(found);
            source.Position += found.Length;
        }
        CompareRange(source.Length);
        return Convert.ToHexString(hash.GetHashAndReset());

        void CompareRange(long end)
        {
            while (source.Position < end)
            {
                int count = (int)Math.Min(end - source.Position, expected.Length);
                source.ReadExactly(expected.AsSpan(0, count));
                copy.ReadExactly(actual.AsSpan(0, count));
                if (!expected.AsSpan(0, count).SequenceEqual(actual.AsSpan(0, count)))
                    throw new InvalidDataException("Bytes outside the replacement segment changed.");
                hash.AppendData(actual, 0, count);
            }
        }
    }

    private static ReadOnlySpan<byte> DdsPayload(byte[] dds)
    {
        var info = DdsTextureReader.Inspect(dds);
        return dds.AsSpan(info.DataOffset, info.DataSize);
    }
}
