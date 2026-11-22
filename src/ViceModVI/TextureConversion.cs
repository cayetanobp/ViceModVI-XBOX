using System.Buffers.Binary;
using BCnEncoder.Encoder;
using BCnEncoder.Shared;
using ImageMagick;
using RpfPreview.Core;

namespace ViceModVI;

public sealed record TextureShape(int Width, int Height, int DxgiFormat, int Mips)
{
    public static TextureShape From(DdsTextureInfo texture) =>
        new(texture.Width, texture.Height, texture.DxgiFormat, texture.Mips);

    public override string ToString() => $"{Width} × {Height} · DXGI {DxgiFormat} · {Mips} mips";
}

public sealed record TextureConversionPlan(TextureShape Input, TextureShape Output, bool RequiresResize,
    bool RequiresConversion, IReadOnlyList<string> Warnings, string? Limitation);

public static class TextureConversion
{
    private const int MaxFileBytes = 256 * 1024 * 1024;
    private const int MaxPixels = 16 * 1024 * 1024;

    private static readonly Dictionary<string, MagickFormat> ImageFormats = new(StringComparer.OrdinalIgnoreCase)
    {
        [".png"] = MagickFormat.Png, [".apng"] = MagickFormat.Png,
        [".jpg"] = MagickFormat.Jpeg, [".jpeg"] = MagickFormat.Jpeg, [".jfif"] = MagickFormat.Jpeg,
        [".bmp"] = MagickFormat.Bmp, [".dib"] = MagickFormat.Bmp, [".gif"] = MagickFormat.Gif,
        [".tiff"] = MagickFormat.Tiff, [".tif"] = MagickFormat.Tiff, [".tga"] = MagickFormat.Tga,
        [".webp"] = MagickFormat.WebP, [".psd"] = MagickFormat.Psd, [".psb"] = MagickFormat.Psb,
        [".avif"] = MagickFormat.Avif, [".heic"] = MagickFormat.Heic, [".heif"] = MagickFormat.Heic,
        [".jxl"] = MagickFormat.Jxl, [".jp2"] = MagickFormat.Jp2, [".j2k"] = MagickFormat.J2k,
        [".qoi"] = MagickFormat.Qoi, [".ico"] = MagickFormat.Ico, [".exr"] = MagickFormat.Exr,
        [".hdr"] = MagickFormat.Hdr, [".pbm"] = MagickFormat.Pbm, [".pgm"] = MagickFormat.Pgm,
        [".ppm"] = MagickFormat.Ppm, [".pnm"] = MagickFormat.Pnm, [".pam"] = MagickFormat.Pam,
        [".pcx"] = MagickFormat.Pcx, [".sgi"] = MagickFormat.Sgi, [".xcf"] = MagickFormat.Xcf
    };

    public static string FileFilter
    {
        get
        {
            string patterns = string.Join(';', ImageFormats.Keys.Order(StringComparer.Ordinal).Select(ext => "*" + ext));
            return $"Texture images|*.dds;{patterns}|All files|*.*";
        }
    }

    public static byte[] Import(string path)
    {
        using var input = File.OpenRead(path);
        if (input.Length is <= 0 or > MaxFileBytes)
            throw new InvalidDataException("Replacement files must be between 1 byte and 256 MiB.");
        var bytes = new byte[(int)input.Length];
        input.ReadExactly(bytes);
        if (bytes.AsSpan().StartsWith("DDS "u8))
        {
            try
            {
                DdsTextureReader.Inspect(bytes);
                return bytes;
            }
            catch (NotSupportedException)
            {
                return ReadRaster(bytes, MagickFormat.Dds);
            }
            catch (InvalidDataException error) when (error.Message == "Unsupported BC format.")
            {
                return ReadRaster(bytes, MagickFormat.Dds);
            }
        }
        string extension = Path.GetExtension(path);
        if (extension.Equals(".dds", StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("The file does not start with the DDS signature.");
        if (!ImageFormats.TryGetValue(extension, out var format))
            throw new NotSupportedException($"No raster decoder is configured for '{extension}'. Choose a supported image format.");
        return ReadRaster(bytes, format);
    }

    private static byte[] ReadRaster(byte[] bytes, MagickFormat format)
    {
        try
        {
            var settings = new MagickReadSettings { Format = format, FrameCount = 1 };
            var info = new MagickImageInfo(bytes, settings);
            CheckPixelCount((int)info.Width, (int)info.Height, "Imported image");
            using var image = new MagickImage(bytes, settings);
            image.AutoOrient();
            image.ColorSpace = ColorSpace.sRGB;
            byte[] pixels = Pixels(image);
            return BuildDds(new((int)image.Width, (int)image.Height, 91, 1), [pixels]);
        }
        catch (MagickException error)
        {
            throw new InvalidDataException($"Could not decode {format}: {error.Message}", error);
        }
    }

    public static TextureConversionPlan Plan(byte[] dds, TextureShape output, string? limitation = null)
    {
        var info = DdsTextureReader.Inspect(dds);
        var input = TextureShape.From(info);
        bool resize = input.Width != output.Width || input.Height != output.Height;
        bool conversion = input != output || info.AlphaMode is not (0 or 1);
        bool keepMips = !resize && input.DxgiFormat == output.DxgiFormat && info.AlphaMode is (0 or 1);
        bool needsEncoding = conversion && (!keepMips || input.Mips < output.Mips);
        if (info.Dimension != 1 || info.Depth != 1 || info.SliceCount != 1 || info.ResourceDimension != 3)
            limitation ??= $"The writer requires one 2D image. The imported DDS contains {info.Shape}.";
        if (info.AlphaMode is not (0 or 1 or 2 or 3))
            limitation ??= $"DDS alpha mode {info.AlphaMode} has no defined conversion to straight alpha.";
        if (needsEncoding)
        {
            if (input.DxgiFormat is 81 or 84)
                limitation ??= $"Decoding DXGI {input.DxgiFormat} signed BC pixels for conversion is unavailable.";
            if (BytesPerElement(output.DxgiFormat) == 0)
                limitation ??= $"Encoding DXGI {output.DxgiFormat} is unavailable. A matching native DDS can be used directly.";
            if ((long)input.Width * input.Height > MaxPixels || (long)output.Width * output.Height > MaxPixels)
                limitation ??= "Texture conversion supports up to 16 megapixels at mip 0.";
        }
        var warnings = new List<string>();
        if (resize) warnings.Add("Resizing changes the sampled detail. A change of aspect ratio can stretch the texture.");
        if (input.DxgiFormat != output.DxgiFormat)
            warnings.Add("Changing DXGI format can alter precision, compressed detail, or alpha coverage.");
        if (needsEncoding && input.DxgiFormat is 10 or 34 or 41 or 54 or 56 or 95 or 96)
            warnings.Add("Conversion samples the image at 8-bit precision and clips values outside 0–1.");
        if (needsEncoding && output.Mips > 1)
            warnings.Add(keepMips ? "Missing mip levels are generated from the last imported mip."
                : "The output mip chain is regenerated from mip 0.");
        if (keepMips && input.Mips > output.Mips)
            warnings.Add("The output keeps the first mip levels needed by the pool.");
        return new(input, output, resize, conversion, warnings, limitation);
    }

    public static byte[] Apply(byte[] dds, TextureConversionPlan plan, bool allowResize = false)
    {
        var info = DdsTextureReader.Inspect(dds);
        if (TextureShape.From(info) != plan.Input)
            throw new InvalidDataException("The input DDS changed after the conversion was planned.");
        if (plan.Limitation is not null) throw new NotSupportedException(plan.Limitation);
        if (plan.RequiresResize && !allowResize)
            throw new InvalidDataException($"The writer requires {plan.Output.Width} × {plan.Output.Height}; " +
                $"the input is {plan.Input.Width} × {plan.Input.Height}. Confirm resizing with --resize.");
        if (!plan.RequiresConversion) return dds;
        var target = plan.Output;
        var mipLevels = new byte[target.Mips][];
        int reused = 0;
        if (!plan.RequiresResize && info.DxgiFormat == target.DxgiFormat && info.AlphaMode is (0 or 1))
        {
            reused = Math.Min(info.Mips, target.Mips);
            for (int i = 0; i < reused; i++)
            {
                var mip = info.Subresources[i];
                mipLevels[i] = dds.AsSpan(mip.Offset, mip.Size).ToArray();
            }
            if (reused == target.Mips) return BuildDds(target, mipLevels, DdsElementSize(info), info.AlphaMode);
        }

        var seed = info.Subresources[Math.Max(0, reused - 1)];
        var seedShape = new TextureShape(seed.Width, seed.Height, info.DxgiFormat, 1);
        byte[] topDds = BuildDds(seedShape, [dds.AsSpan(seed.Offset, seed.Size).ToArray()],
            DdsElementSize(info), info.AlphaMode);
        byte[] pixels = DdsTextureReader.DecodeTexturePixels(topDds).Bgra32;
        if (info.DxgiFormat == 65)
        {
            for (int offset = 0; offset < pixels.Length; offset += 4)
            {
                pixels[offset + 3] = pixels[offset];
                pixels[offset] = pixels[offset + 1] = pixels[offset + 2] = 255;
            }
        }
        if (info.AlphaMode == 3)
            for (int offset = 3; offset < pixels.Length; offset += 4) pixels[offset] = 255;

        var settings = new PixelReadSettings((uint)seed.Width, (uint)seed.Height, StorageType.Char, PixelMapping.BGRA);
        using var image = new MagickImage(pixels, settings);
        image.FilterType = FilterType.Triangle;
        if (plan.RequiresResize) Resize(image, target.Width, target.Height);

        image.FilterType = FilterType.Box;
        for (int mip = reused; mip < mipLevels.Length; mip++)
        {
            int width = Math.Max(1, target.Width >> mip);
            int height = Math.Max(1, target.Height >> mip);
            if (mip > 0) Resize(image, width, height);
            mipLevels[mip] = EncodeMip(Pixels(image), width, height, target.DxgiFormat);
            int block = IsBc(target.DxgiFormat) ? 4 : 1;
            int expected = ((width + block - 1) / block) * ((height + block - 1) / block) * BytesPerElement(target.DxgiFormat);
            if (mipLevels[mip].Length != expected)
                throw new InvalidDataException($"The DDS encoder produced the wrong byte count for mip {mip}.");
        }
        return BuildDds(target, mipLevels, alphaMode: 1);
    }

    private static void CheckPixelCount(int width, int height, string label)
    {
        if (width is <= 0 or > 16384 || height is <= 0 or > 16384 || (long)width * height > MaxPixels)
            throw new NotSupportedException($"{label} exceeds the 16 megapixel or 16,384 pixels per side limit.");
    }

    private static void Resize(IMagickImage<byte> image, int width, int height) =>
        image.Resize(new MagickGeometry((uint)width, (uint)height) { IgnoreAspectRatio = true });

    private static byte[] Pixels(IMagickImage<byte> image)
    {
        using var pixels = image.GetPixels();
        return pixels.ToByteArray(PixelMapping.BGRA) ?? throw new InvalidDataException("No pixels were decoded.");
    }

    private static bool IsBc(int dxgi) => dxgi is >= 70 and <= 84 or >= 94 and <= 99;

    private static int DdsElementSize(DdsTextureInfo info)
    {
        int block = IsBc(info.DxgiFormat) ? 4 : 1;
        return info.Subresources[0].RowBytes / ((info.Width + block - 1) / block);
    }

    private static int BytesPerElement(int dxgi) => dxgi switch
    {
        61 or 65 => 1,
        49 or 54 or 56 => 2,
        24 or 28 or 29 or 34 or 41 or 87 or 88 or 91 or 93 => 4,
        10 or 71 or 72 or 80 => 8,
        74 or 75 or 77 or 78 or 83 or 98 or 99 => 16,
        _ => 0
    };

    private static byte[] EncodeMip(byte[] bgra, int width, int height, int format)
    {
        if (IsBc(format))
        {
            var compression = format switch
            {
                71 or 72 => CompressionFormat.Bc1WithAlpha,
                74 or 75 => CompressionFormat.Bc2,
                77 or 78 => CompressionFormat.Bc3,
                80 => CompressionFormat.Bc4,
                83 => CompressionFormat.Bc5,
                98 or 99 => CompressionFormat.Bc7,
                _ => throw new NotSupportedException($"Encoding DXGI {format} is unavailable.")
            };
            var encoder = new BcEncoder(compression);
            encoder.OutputOptions.GenerateMipMaps = false;
            encoder.OutputOptions.Quality = CompressionQuality.Balanced;
            return encoder.EncodeToRawBytes(bgra, width, height, PixelFormat.Bgra32)[0];
        }

        if (format is 87 or 91) return bgra;
        int stride = BytesPerElement(format);
        var pixels = new byte[width * height * stride];
        for (int i = 0, j = 0; i < bgra.Length; i += 4, j += stride)
        {
            byte b = bgra[i], g = bgra[i + 1], r = bgra[i + 2], a = bgra[i + 3];
            switch (format)
            {
                case 28 or 29:
                    pixels[j] = r; pixels[j + 1] = g; pixels[j + 2] = b; pixels[j + 3] = a;
                    break;
                case 88 or 93:
                    pixels[j] = b; pixels[j + 1] = g; pixels[j + 2] = r; pixels[j + 3] = 255;
                    break;
                case 61: pixels[j] = r; break;
                case 65: pixels[j] = a; break;
                case 49: pixels[j] = r; pixels[j + 1] = g; break;
                case 56: BinaryPrimitives.WriteUInt16LittleEndian(pixels.AsSpan(j), (ushort)(r * 257)); break;
                case 41: BinaryPrimitives.WriteSingleLittleEndian(pixels.AsSpan(j), r / 255f); break;
                case 54: HalfAt(j, r); break;
                case 34: HalfAt(j, r); HalfAt(j + 2, g); break;
                case 10: HalfAt(j, r); HalfAt(j + 2, g); HalfAt(j + 4, b); HalfAt(j + 6, a); break;
                case 24:
                    uint packed = (uint)((r * 1023 + 127) / 255) | (uint)((g * 1023 + 127) / 255) << 10 |
                        (uint)((b * 1023 + 127) / 255) << 20 | (uint)((a * 3 + 127) / 255) << 30;
                    BinaryPrimitives.WriteUInt32LittleEndian(pixels.AsSpan(j), packed);
                    break;
                default: throw new NotSupportedException($"Encoding DXGI {format} is unavailable.");
            }
        }
        return pixels;

        void HalfAt(int offset, byte value) =>
            BinaryPrimitives.WriteUInt16LittleEndian(pixels.AsSpan(offset), BitConverter.HalfToUInt16Bits((System.Half)(value / 255f)));
    }

    internal static byte[] BuildDds(TextureShape shape, IReadOnlyList<byte[]> levels, int elementSize = 0, int alphaMode = 0)
    {
        bool bc = IsBc(shape.DxgiFormat);
        using var output = new MemoryStream(148 + levels.Sum(level => level.Length));
        using var writer = new BinaryWriter(output);
        writer.Write("DDS "u8);
        writer.Write(124);
        writer.Write(0x1007 | (bc ? 0x80000 : 8) | (shape.Mips > 1 ? 0x20000 : 0));
        writer.Write(shape.Height);
        writer.Write(shape.Width);
        writer.Write(bc ? levels[0].Length : shape.Width * (elementSize > 0 ? elementSize : BytesPerElement(shape.DxgiFormat)));
        writer.Write(0);
        writer.Write(shape.Mips);
        writer.Write(new byte[44]);
        writer.Write(32);
        writer.Write(4);
        writer.Write("DX10"u8);
        writer.Write(new byte[20]);
        writer.Write(shape.Mips > 1 ? 0x401008 : 0x1000);
        writer.Write(new byte[16]);
        writer.Write(shape.DxgiFormat);
        writer.Write(3);
        writer.Write(0);
        writer.Write(1);
        writer.Write(alphaMode);
        foreach (byte[] level in levels) writer.Write(level);
        byte[] bytes = output.ToArray();
        DdsTextureReader.Inspect(bytes);
        return bytes;
    }
}
