using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using RpfPreview.Core;

namespace ViceModVI;

internal static class BcpackTextureReader
{
    public static (byte[] Dds, int BaseMip) CreateDds(string path, TtpfTextureInfo texture,
        CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        string script = Path.Combine(AppContext.BaseDirectory, "ttpf-tools", "preview_dds.py");
        if (!File.Exists(script))
            throw new NotSupportedException("This texture requires BCPack. Build with the Leonida texture tools to read it.");

        var directory = Directory.CreateTempSubdirectory("vicemodvi-ttpf-");
        string output = Path.Combine(directory.FullName, "texture.dds");
        try
        {
            Decode(script, path, texture.Index, output, cancellationToken);
            var file = new FileInfo(output);
            if (!file.Exists || file.Length is < 148 or > DdsTextureReader.MaxFileBytes)
                throw new InvalidDataException("The BCPack decoder did not produce a bounded DDS.");

            byte[] bytes = File.ReadAllBytes(output);
            var dds = DdsTextureReader.Inspect(bytes);
            var baseMip = 0;
            while (baseMip < texture.SourceMips &&
                (dds.Width != Math.Max(1, texture.Width >> baseMip) || dds.Height != Math.Max(1, texture.Height >> baseMip)))
                baseMip++;
            if (dds.ResourceDimension != 3 || dds.Dimension != 1 || dds.SliceCount != 1 ||
                dds.DxgiFormat != texture.DxgiFormat || dds.Mips > texture.SourceMips - baseMip)
                throw new InvalidDataException("The BCPack DDS does not match the source texture descriptor.");
            return (bytes, baseMip);
        }
        finally
        {
            File.Delete(output);
            directory.Delete();
        }
    }

    private static void Decode(string script, string path, int index, string output, CancellationToken cancellationToken)
    {
        using var process = new Process
        {
            StartInfo = new(OperatingSystem.IsWindows() ? "python.exe" : "python3")
            {
                UseShellExecute = false,
                CreateNoWindow = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true
            }
        };
        foreach (string argument in new[] { "-IB", script, path, index.ToString(CultureInfo.InvariantCulture), output })
            process.StartInfo.ArgumentList.Add(argument);
        try
        {
            if (!process.Start()) throw new IOException("Could not start the BCPack decoder.");
        }
        catch (Win32Exception error)
        {
            throw new NotSupportedException("BCPack decoding requires Python 3 on PATH.", error);
        }

        var stdout = process.StandardOutput.BaseStream.CopyToAsync(Stream.Null);
        var stderr = process.StandardError.ReadToEndAsync();
        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        timeout.CancelAfter(TimeSpan.FromSeconds(30));
        try
        {
            process.WaitForExitAsync(timeout.Token).GetAwaiter().GetResult();
        }
        catch (OperationCanceledException)
        {
            try { process.Kill(); }
            catch (InvalidOperationException) { }
            catch (Win32Exception) when (process.HasExited) { }
            process.WaitForExit();
            cancellationToken.ThrowIfCancellationRequested();
            throw new NotSupportedException("The BCPack decoder did not finish within 30 seconds.");
        }

        stdout.GetAwaiter().GetResult();
        string diagnostics = stderr.GetAwaiter().GetResult().Trim();
        cancellationToken.ThrowIfCancellationRequested();
        if (process.ExitCode == 0) return;

        string reason = diagnostics.Split('\n', StringSplitOptions.TrimEntries | StringSplitOptions.RemoveEmptyEntries)
            .LastOrDefault() ?? "The decoder did not return an explanation.";
        throw new NotSupportedException($"Strict BCPack decoding failed. {reason}");
    }
}
