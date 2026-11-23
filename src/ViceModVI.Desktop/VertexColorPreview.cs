using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Media.Media3D;
using RpfPreview.Core;

namespace ViceModVI.Desktop;

internal static class VertexColorPreview
{
    public static (MeshGeometry3D Mesh, BitmapSource Image)? Create(RageMesh mesh)
    {
        const int tile = 6;
        const int cell = tile + 2;
        const int pixelLimit = 4_000_000;
        if (mesh.VertexColors is not { } colors || colors.Count != mesh.Positions.Count) return null;

        int triangles = mesh.Indices.Count / 3;
        if (triangles < 1 || triangles > pixelLimit / (cell * cell) || mesh.Indices.Count % 3 != 0) return null;
        int columns = (int)Math.Ceiling(Math.Sqrt(triangles));
        int rows = (triangles + columns - 1) / columns;
        int width = columns * cell, height = rows * cell;
        if (width * height > pixelLimit) return null;

        byte[] pixels = new byte[width * height * 4];
        var geometry = new MeshGeometry3D();
        for (int face = 0; face < triangles; face++)
        {
            int first = face * 3;
            int x = face % columns * cell + 1, y = face / columns * cell + 1;
            for (int corner = 0; corner < 3; corner++)
            {
                var position = mesh.Positions[mesh.Indices[first + corner]];
                geometry.Positions.Add(new(position.X, position.Y, position.Z));
                geometry.TriangleIndices.Add(first + corner);
                geometry.TextureCoordinates.Add(new((x + 0.5 + (corner == 1 ? tile - 1 : 0)) / width,
                    (y + 0.5 + (corner == 2 ? tile - 1 : 0)) / height));
            }

            uint a = colors[mesh.Indices[first]];
            uint b = colors[mesh.Indices[first + 1]];
            uint c = colors[mesh.Indices[first + 2]];
            for (int dy = -1; dy <= tile; dy++)
            for (int dx = -1; dx <= tile; dx++)
            {
                double u = (double)dx / (tile - 1), v = (double)dy / (tile - 1);
                int target = ((y + dy) * width + x + dx) * 4;
                for (int channel = 0; channel < 3; channel++)
                {
                    int shift = 8 * channel;
                    double value = ((a >> shift) & 255) * (1 - u - v) +
                        ((b >> shift) & 255) * u + ((c >> shift) & 255) * v;
                    pixels[target + 2 - channel] = (byte)Math.Clamp((int)Math.Round(value), 0, 255);
                }
                pixels[target + 3] = 255;
            }
        }
        geometry.Freeze();
        var image = BitmapSource.Create(width, height, 96, 96, PixelFormats.Bgra32, null, pixels, width * 4);
        image.Freeze();
        return (geometry, image);
    }
}
