using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;
using System.Globalization;
using System.Numerics;
using RpfPreview.Core;

namespace ViceModVI;

internal static class ResourceModels
{
    public static bool Available => File.Exists(Path.Combine(AppContext.BaseDirectory, "vicemodvi_models.dll"));

    internal sealed record Inventory(IReadOnlyList<long>? CandidateOffsets, IReadOnlyList<string> Rows);

    public static Inventory List(Source source, Action<IndexProgress>? progress = null,
        CancellationToken cancellationToken = default)
    {
        using var output = ReadOutput(source.Path, null, progress, cancellationToken);
        using var reader = new StreamReader(output);
        var offsets = new List<long>();
        var rows = new List<string>();
        bool indexed = reader.Peek() == '#';
        if (indexed && reader.ReadLine() != "# VMI1") throw new InvalidDataException("Unknown model index format.");
        while (reader.ReadLine() is { } line)
        {
            if (line.StartsWith("offset\t", StringComparison.Ordinal))
                offsets.Add(long.Parse(line.AsSpan(7), CultureInfo.InvariantCulture));
            else rows.Add(line);
        }
        return new(indexed ? offsets : null, rows);
    }

    public static void AddDrawables(Catalog catalog, Source source, int firstAsset, IReadOnlyList<string> rows)
    {
        var legacy = catalog.Assets.Skip(firstAsset)
            .Where(a => a.Type == "model" && a.Range is not null).ToLookup(a => a.Range!.Offset);
        var matched = new HashSet<string>();
        foreach (string line in rows)
        {
            var parts = line.Split('\t', 5);
            if (parts.Length != 5) throw new InvalidDataException("Invalid drawable list.");
            long offset = long.Parse(parts[0], CultureInfo.InvariantCulture);
            long end = long.Parse(parts[1], CultureInfo.InvariantCulture);
            int index = int.Parse(parts[2], CultureInfo.InvariantCulture);
            var range = new ByteRange(offset, checked(end - offset));
            range.Validate(source.Fingerprint.Length);
            string name = DrawableName(parts[3]);
            var existing = legacy[offset].FirstOrDefault(a => !matched.Contains(a.Id) &&
                string.Equals(DrawableName(a.Name), name, StringComparison.OrdinalIgnoreCase));
            if (existing is not null)
            {
                matched.Add(existing.Id);
                continue;
            }
            catalog.Assets.Add(new($"{source.Id}:drawable:{offset:X}:{index}", source.Id, "model",
                name, "resource", "Resource Tools drawable", Range: range, DrawableLocator: parts[4]));
        }
    }

    private static string DrawableName(string name) =>
        name.EndsWith(".#dr", StringComparison.OrdinalIgnoreCase) ? name[..^4] : name;

    public static RageModel Read(Source source, Asset asset)
    {
        if (!Available) throw new NotSupportedException("The integrated model reader is missing from this build.");
        using var output = ReadOutput(source.Path, asset.DrawableLocator, null, CancellationToken.None);
        using var mesh = new BinaryReader(output);
        ReadOnlySpan<byte> version = mesh.ReadBytes(4);
        bool hasMaterials = version.SequenceEqual("VMM2"u8);
        if (!hasMaterials && !version.SequenceEqual("VMM1"u8))
            throw new InvalidDataException("Invalid drawable data.");
        IReadOnlyList<RageMaterial> materials = [];
        if (hasMaterials)
        {
            int drawable = mesh.ReadInt32();
            int length = mesh.ReadInt32();
            if (drawable < 0 || length < 16 || drawable > length - 16 ||
                length > 256 * 1024 * 1024 || length > output.Length - output.Position - 4)
                throw new InvalidDataException("Invalid drawable material data.");
            materials = DrawableMaterials.Read(mesh.ReadBytes(length), drawable);
        }
        int count = mesh.ReadInt32();
        if (count is < 1 or > 65536) throw new InvalidDataException("Invalid mesh count.");
        var meshes = new List<RageMesh>(count);
        var min = new Vector3(float.PositiveInfinity);
        var max = new Vector3(float.NegativeInfinity);
        for (int part = 0; part < count; part++)
        {
            int verts = mesh.ReadInt32(), triangles = mesh.ReadInt32(), uv = mesh.ReadInt32(), shader = mesh.ReadInt32();
            long required = (12L + uv * 8L) * verts + 12L * triangles;
            if (verts < 1 || triangles < 1 || uv is < 0 or > 1 || required > output.Length - output.Position)
                throw new InvalidDataException("Invalid drawable mesh lengths.");
            var positions = new Vector3[verts];
            for (int i = 0; i < verts; i++)
            {
                var position = new Vector3(mesh.ReadSingle(), mesh.ReadSingle(), mesh.ReadSingle());
                positions[i] = position;
                min = Vector3.Min(min, position);
                max = Vector3.Max(max, position);
            }
            var indices = new int[checked(triangles * 3)];
            for (int i = 0; i < indices.Length; i++)
            {
                indices[i] = mesh.ReadInt32();
                if ((uint)indices[i] >= (uint)verts) throw new InvalidDataException("Invalid drawable mesh index.");
            }
            Vector2[]? coords = uv == 1 ? new Vector2[verts] : null;
            if (coords is not null)
                for (int i = 0; i < verts; i++) coords[i] = new(mesh.ReadSingle(), mesh.ReadSingle());
            meshes.Add(new($"Mesh {part + 1}", positions, indices) { ShaderIndex = shader, TexCoords = coords });
        }
        return new(asset.Name, meshes, min, max) { Materials = materials, PresentationNote = "Static geometry from Resource Tools." };
    }

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate int Output(IntPtr data, nuint length);

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate int Progress(IntPtr phase, nuint phaseLength, long done, long total,
        IntPtr unit, nuint unitLength);

    [DllImport("vicemodvi_models", EntryPoint = "vicemodvi_read_model", CallingConvention = CallingConvention.Cdecl)]
    private static extern int ReadNative(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string path,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? locator,
        Output output, Output error, Progress progress);

    private static MemoryStream ReadOutput(string path, string? locator, Action<IndexProgress>? progress,
        CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        byte[]? bytes = null;
        string? diagnostics = null;
        ExceptionDispatchInfo? callbackError = null;

        int Invoke(Action callback)
        {
            if (cancellationToken.IsCancellationRequested) return 0;
            try
            {
                callback();
                return 1;
            }
            catch (Exception exception)
            {
                callbackError = ExceptionDispatchInfo.Capture(exception);
                return 0;
            }
        }

        static string Utf8(IntPtr pointer, nuint length) =>
            Marshal.PtrToStringUTF8(pointer, checked((int)length));

        int result = ReadNative(path, locator,
            (data, length) => Invoke(() =>
            {
                bytes = new byte[checked((int)length)];
                Marshal.Copy(data, bytes, 0, bytes.Length);
            }),
            (data, length) => Invoke(() => diagnostics = Utf8(data, length)),
            (phase, phaseLength, done, total, unit, unitLength) => Invoke(() =>
                progress?.Invoke(new(Utf8(phase, phaseLength), done, total, Utf8(unit, unitLength)))));
        callbackError?.Throw();
        cancellationToken.ThrowIfCancellationRequested();
        if (result != 0 || bytes is null)
            throw new InvalidDataException(diagnostics ?? "The native model reader returned no data.");
        return new MemoryStream(bytes, writable: false);
    }
}
