using System.Diagnostics;
using System.Globalization;
using System.Numerics;
using RpfPreview.Core;

namespace ViceModVI;

internal static class ResourceModels
{
    private static string Helper => Path.Combine(AppContext.BaseDirectory,
        OperatingSystem.IsWindows() ? "vicemodvi-models.exe" : "vicemodvi-models");

    public static bool Available => File.Exists(Helper);

    internal sealed record Inventory(IReadOnlyList<long>? CandidateOffsets, IReadOnlyList<string> Rows);

    public static Inventory List(Source source, Action<IndexProgress>? progress = null,
        CancellationToken cancellationToken = default)
    {
        using var output = ReadOutput(progress, cancellationToken, "list", source.Path);
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
        if (!Available) throw new NotSupportedException("Place vicemodvi-models next to ViceModVI to read this drawable.");
        using var output = ReadOutput(null, CancellationToken.None, "mesh", source.Path, asset.DrawableLocator!);
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

    private static async Task<string> ReadDiagnostics(StreamReader input, Action<IndexProgress>? progress)
    {
        var diagnostics = new System.Text.StringBuilder();
        while (await input.ReadLineAsync().ConfigureAwait(false) is { } line)
        {
            var fields = line.Split('\t');
            if (fields.Length == 5 && fields[0] == "VMP1" &&
                long.TryParse(fields[2], out long done) && long.TryParse(fields[3], out long total))
                progress?.Invoke(new(fields[1], done, total, fields[4]));
            else diagnostics.AppendLine(line);
        }
        return diagnostics.ToString();
    }

    private static MemoryStream ReadOutput(Action<IndexProgress>? progress, CancellationToken cancellationToken, params string[] args)
    {
        cancellationToken.ThrowIfCancellationRequested();
        var start = new ProcessStartInfo(Helper)
        {
            UseShellExecute = false, CreateNoWindow = true,
            RedirectStandardOutput = true, RedirectStandardError = true
        };
        foreach (string arg in args) start.ArgumentList.Add(arg);
        using var process = Process.Start(start) ?? throw new IOException("Could not start the drawable reader.");
        using var cancellation = cancellationToken.Register(() =>
        {
            try { if (!process.HasExited) process.Kill(); }
            catch (InvalidOperationException) { }
            catch (System.ComponentModel.Win32Exception) { }
        });
        var errors = ReadDiagnostics(process.StandardError, progress);
        var data = new MemoryStream();
        try
        {
            process.StandardOutput.BaseStream.CopyTo(data);
            process.WaitForExit();
            string diagnostics = errors.GetAwaiter().GetResult();
            cancellationToken.ThrowIfCancellationRequested();
            if (process.ExitCode != 0) throw new InvalidDataException(diagnostics.Trim());
            data.Position = 0;
            return data;
        }
        catch
        {
            data.Dispose();
            throw;
        }
    }
}
