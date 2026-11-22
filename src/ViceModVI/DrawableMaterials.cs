using System.Buffers.Binary;
using RpfPreview.Core;

namespace ViceModVI;

internal static class DrawableMaterials
{
    public static IReadOnlyList<RageMaterial> Read(byte[] data, int drawable)
    {
        int Pointer(int at, int size)
        {
            ulong address = U64(at);
            if (address < 0x50000000 || address - 0x50000000 > (ulong)data.Length)
                throw new InvalidDataException("Invalid drawable material pointer.");
            int offset = (int)(address - 0x50000000);
            if (size > data.Length - offset)
                throw new InvalidDataException("Truncated drawable material.");
            return offset;
        }
        ulong U64(int at) => BinaryPrimitives.ReadUInt64LittleEndian(data.AsSpan(at));
        uint U32(int at) => BinaryPrimitives.ReadUInt32LittleEndian(data.AsSpan(at));
        ushort U16(int at) => BinaryPrimitives.ReadUInt16LittleEndian(data.AsSpan(at));

        var materials = new List<RageMaterial>();
        try
        {
            int group = Pointer(drawable + 16, 64);
            int count = U16(group + 24);
            if (count is < 1 or > 512 || count != U16(group + 26)) return materials;
            int shaders = Pointer(group + 16, count * 8);
            for (int i = 0; i < count; i++)
            {
                try
                {
                    int shader = Pointer(shaders + i * 8, 64);
                    uint hash = U32(shader), preset = U32(shader + 4);
                    if (preset != 0x6D657461) continue;
                    int info = Pointer(shader + 32, 8);
                    int textures = data[info + 1], parameters = data[info + 4];
                    if (data[info + 5] != 0 || data[info + 6] != 0 || data[info + 7] is < 1 or > 16 ||
                        info > data.Length - (8 + parameters * 8)) continue;
                    int table = textures > 0 ? Pointer(shader + 16, textures * 8) : 0;
                    var refs = new List<RageTextureReference>();
                    for (int p = 0; p < parameters; p++)
                    {
                        int parameter = info + 8 + p * 8;
                        uint binding = U32(parameter + 4);
                        int slot = (int)((binding >> 2) & 255);
                        if ((binding & 3) != 0 || slot >= textures || U64(table + slot * 8) == 0) continue;
                        int texture = Pointer(table + slot * 8, 80);
                        if (U32(texture + 16) != 0x980 || U32(texture + 20) != 0 ||
                            U64(texture + 24) != 0 || U64(texture + 40) != 0 || U32(texture + 56) != 0 ||
                            U16(texture + 60) != 1 || U16(texture + 62) != 1 || data[texture + 64] != 255 ||
                            U64(texture + 48) == 0) continue;
                        int asset = Pointer(texture + 48, 16);
                        if (asset < 16 || U32(asset - 16) != 6 || U32(asset - 12) != 0 ||
                            U32(asset - 4) != 0 || U64(asset) != 0x50000000UL + (ulong)asset - 11 ||
                            U64(asset + 8) == 0) continue;
                        refs.Add(new(U32(parameter), slot, texture, asset, U64(asset + 8), U32(asset - 8), 6));
                    }
                    materials.Add(new(i, hash, preset, refs)
                    {
                        RenderBucket = data[shader + 57], RenderBucketMask = U32(shader + 60)
                    });
                }
                catch (InvalidDataException) { /* Unknown shader layout. */ }
            }
        }
        catch (Exception error) when (error is InvalidDataException or ArgumentException)
        {
            // Drawables with an unknown material layout still have usable geometry.
        }
        return materials;
    }
}
