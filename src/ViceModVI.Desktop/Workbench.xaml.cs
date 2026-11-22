using System.ComponentModel;
using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Media.Media3D;
using Microsoft.Win32;
using RpfPreview.Core;

namespace ViceModVI.Desktop;

public partial class Workbench : Window
{
    private Catalog? catalog;
    private ReplacementProject? project;
    private string? projectPath;
    private Asset? selectedAsset;
    private Asset? texture;
    private TextureExport? textureInfo;
    private RageModel? model;
    private TextureLink[] links = [];
    private BitmapSource? originalImage;
    private BitmapSource? replacementImage;
    private bool updating;
    private bool dirty;
    private bool busy;
    private bool closeAfterOpening;
    private CancellationTokenSource? openingCancellation;
    private bool poolWriteAvailable;
    private string? poolWriteLimitation;
    private Point? drag;
    private Point3D center;
    private double distance, yaw, pitch;

    private sealed record MeshChoice(int Index, int? Shader, string Label);
    private sealed record MaterialChoice(int Shader, string Label);

    public Workbench() => InitializeComponent();

    public async Task OpenPath(string path)
    {
        if (!CanDiscard()) return;
        path = Path.GetFullPath(path);
        await Run("Opening…", async () =>
        {
            if (Path.GetExtension(path).Equals(".vmp", StringComparison.OrdinalIgnoreCase))
            {
                var loaded = await Task.Run(() =>
                {
                    var saved = ReplacementProject.Load(path, out var loadedCatalog);
                    return (saved, loadedCatalog);
                });
                SetWorkspace(loaded.loadedCatalog, loaded.saved, path);
            }
            else if (Path.GetExtension(path).Equals(".json", StringComparison.OrdinalIgnoreCase) && File.Exists(path))
            {
                var loaded = await Task.Run(() => Catalog.Load(path));
                SetWorkspace(loaded, new() { CatalogPath = path });
            }
            else await OpenSources(path);
        });
    }

    private void SetWorkspace(Catalog loaded, ReplacementProject replacementProject, string? path = null)
    {
        catalog = loaded;
        project = replacementProject;
        projectPath = path;
        dirty = false;
        updating = true;
        Search.Clear();
        TypeFilter.SelectedIndex = 0;
        updating = false;
        FilterChanged(this, new RoutedEventArgs());
        LoadAsset(null);
        var savedAsset = loaded.Assets.SingleOrDefault(asset => asset.Id == replacementProject.SelectedAssetId);
        if (savedAsset is not null)
        {
            Search.Text = savedAsset.Id;
            AssetList.SelectedItem = savedAsset;
            if (replacementProject.SelectedMesh is int index && Meshes.IsVisible && index < Meshes.Items.Count && index >= 0)
                Meshes.SelectedIndex = index;
            if (replacementProject.ShowReplacement && replacementImage is not null)
                ShowReplacement.IsChecked = true;
            RefreshPreview();
        }
        SaveProjectButton.IsEnabled = true;
        int ranges = loaded.Assets.Count(asset => asset.Type == "unknown");
        int models = loaded.Assets.Count(asset => asset.Type == "model");
        StatusLine.Text = $"{models:N0} models · " +
            $"{loaded.Assets.Count - ranges:N0} identified entries and {ranges:N0} unknown ranges. " +
            $"{replacementProject.Replacements.Count} staged replacements.";
    }

    private static string TemporaryCatalog => Path.Combine(Path.GetTempPath(), "vicemodvi-catalog.json");

    private async void OpenFolder(object sender, RoutedEventArgs e)
    {
        var dialog = new OpenFolderDialog { Title = "Open Xbox Series asset folder" };
        if (dialog.ShowDialog(this) == true) await OpenPath(dialog.FolderName);
    }

    private async void OpenFile(object sender, RoutedEventArgs e)
    {
        var dialog = new OpenFileDialog { Filter = "Xbox Series assets|*.rpf;*.ttpf|RPF archives|*.rpf|TTPF textures|*.ttpf" };
        if (dialog.ShowDialog(this) == true) await OpenPath(dialog.FileName);
    }

    private async Task OpenSources(string path)
    {
        using var cancellation = new CancellationTokenSource();
        openingCancellation = cancellation;
        WorkProgress.Value = 0;
        WorkProgress.Visibility = Visibility.Visible;
        WorkProgress.IsIndeterminate = true;
        var progress = (IProgress<(string Path, IndexProgress Entry)>)new Progress<(string Path, IndexProgress Entry)>(update =>
        {
            if (cancellation.IsCancellationRequested || WorkProgress.Visibility != Visibility.Visible) return;
            var entry = update.Entry;
            WorkProgress.IsIndeterminate = entry.Total <= 0;
            WorkProgress.Maximum = Math.Max(1, entry.Total);
            WorkProgress.Value = entry.Completed;
            string count = entry.Total <= 0 ? "" : $" · {entry.Completed:N0}/{entry.Total:N0} {entry.Unit}";
            StatusLine.Text = $"{Path.GetFileName(update.Path)} · {entry.Phase}{count}";
        });
        try
        {
            var loaded = await Task.Run(() =>
            {
                IEnumerable<string> paths = Directory.Exists(path)
                    ? Directory.EnumerateFiles(path, "*", SearchOption.AllDirectories)
                        .Where(p => Path.GetExtension(p).ToLowerInvariant() is ".rpf" or ".ttpf")
                        .Order(StringComparer.OrdinalIgnoreCase)
                    : [path];
                var next = XboxAssets.Build(paths, progress: (current, entry) => progress.Report((current, entry)),
                    cancellationToken: cancellation.Token);
                cancellation.Token.ThrowIfCancellationRequested();
                next.Save(TemporaryCatalog, replace: true);
                return next;
            });
            if (!cancellation.IsCancellationRequested) SetWorkspace(loaded, new());
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
        {
            StatusLine.Text = "Opening canceled.";
        }
        finally
        {
            openingCancellation = null;
            WorkProgress.Visibility = Visibility.Collapsed;
        }
    }

    private async void OpenProject(object sender, RoutedEventArgs e)
    {
        var dialog = new OpenFileDialog { Filter = "ViceModVI project|*.vmp" };
        if (dialog.ShowDialog(this) == true) await OpenPath(dialog.FileName);
    }

    private async void SaveProject(object sender, RoutedEventArgs e)
    {
        if (project is null) return;
        string? path = projectPath;
        if (path is null)
        {
            var dialog = new SaveFileDialog { Filter = "ViceModVI project|*.vmp" };
            if (dialog.ShowDialog(this) != true) return;
            path = dialog.FileName;
        }
        await Run("Saving project…", async () =>
        {
            await SaveWorkspace(path);
        });
    }

    private async Task SaveWorkspace(string path)
    {
        project!.SelectedAssetId = selectedAsset?.Id;
        project.SelectedMesh = model is null ? null : (Meshes.SelectedItem as MeshChoice)?.Index;
        project.ShowReplacement = ShowReplacement.IsChecked == true;
        await Task.Run(() => project.Save(path, catalog!));
        projectPath = path;
        dirty = false;
        StatusLine.Text = $"Saved {Path.GetFileName(path)}.";
    }

    private void FilterChanged(object sender, RoutedEventArgs e)
    {
        if (catalog is null || updating) return;
        string query = Search.Text.Trim();
        var type = (TypeFilter.SelectedItem as ComboBoxItem)?.Tag as string;
        var bySource = catalog.Sources.ToDictionary(source => source.Id, source => source.Path);
        string? key = null;
        if (ulong.TryParse(query.StartsWith("0x", StringComparison.OrdinalIgnoreCase) ? query[2..] : query,
                System.Globalization.NumberStyles.AllowHexSpecifier, null, out var parsed))
            key = parsed.ToString("X16");
        var assets = catalog.Find(type: type).Where(asset => query.Length == 0 ||
            asset.Name.Contains(query, StringComparison.OrdinalIgnoreCase) ||
            asset.Id.Contains(query, StringComparison.OrdinalIgnoreCase) ||
            bySource[asset.SourceId].Contains(query, StringComparison.OrdinalIgnoreCase) ||
            (key is not null && (asset.NativeKey == key || asset.NameHash == key ||
                asset.Range?.Offset == (long?)parsed))).ToArray();
        var previous = selectedAsset;
        updating = true;
        AssetList.ItemsSource = assets;
        AssetList.SelectedItem = previous is not null && assets.Contains(previous) ? previous : null;
        updating = false;
        if (AssetList.SelectedItem is null && previous is not null) LoadAsset(null);
    }

    private void SelectAsset(object sender, SelectionChangedEventArgs e)
    {
        if (!updating) LoadAsset(AssetList.SelectedItem as Asset);
    }

    private void LoadAsset(Asset? asset)
    {
        if (catalog is null) return;
        selectedAsset = asset;
        model = null;
        links = [];
        MeshControls.Visibility = Visibility.Collapsed;
        ExportAssetButton.IsEnabled = asset?.Type == "model" || asset?.Range is not null;
        AssetHeading.Text = asset?.Name ?? "Preview";
        AssetDetails.Text = asset is null ? "" :
            $"{asset.Type} · {asset.NameOrigin} name\n{Path.GetFileName(catalog.SourceOf(asset).Path)}\n{asset.Id}\n{asset.Evidence}";
        if (asset?.Range is { } range)
            AssetDetails.Text += $"\n0x{range.Offset:X} · {range.Length:N0} stored bytes";
        LoadTexture(null);
        if (asset is null) return;
        try
        {
            if (asset.Type == "model")
            {
                model = XboxAssets.ReadModel(catalog, asset);
                links = TextureLinks.Resolve(catalog, asset, model).ToArray();
                var choices = model.Meshes.Select((mesh, index) => new MeshChoice(index, mesh.ShaderIndex, mesh.Name)).ToArray();
                var shaderChoices = (model.Materials ?? []).Select(material =>
                    new MaterialChoice(material.ShaderIndex, $"Shader {material.ShaderIndex} ({material.ShaderHash:X8})")).ToArray();
                MeshControls.Visibility = Visibility.Visible;
                updating = true;
                Meshes.DisplayMemberPath = nameof(MeshChoice.Label);
                Meshes.ItemsSource = choices;
                Materials.DisplayMemberPath = nameof(MaterialChoice.Label);
                Materials.ItemsSource = shaderChoices;
                Meshes.SelectedItem = choices.FirstOrDefault(mesh => links.Any(link =>
                    link.Status == "confirmed" && link.Shader == mesh.Shader)) ?? choices.FirstOrDefault();
                updating = false;
                SelectMesh(this, null!);
            }
            else if (asset.Type == "texture") LoadTexture(asset);
        }
        catch (Exception error) when (IsAssetError(error))
        {
            StatusLine.Text = error.Message;
        }
    }

    private void SelectMesh(object sender, SelectionChangedEventArgs e)
    {
        if (updating || Meshes.SelectedItem is not MeshChoice selected) return;
        updating = true;
        Materials.SelectedItem = Materials.Items.OfType<MaterialChoice>().FirstOrDefault(item => item.Shader == selected.Shader);
        updating = false;
        RefreshLink();
        FitMesh();
    }

    private void SelectMaterial(object sender, SelectionChangedEventArgs e)
    {
        if (updating || Materials.SelectedItem is not MaterialChoice selected) return;
        if (Meshes.SelectedItem is not MeshChoice mesh || mesh.Shader != selected.Shader)
        {
            updating = true;
            Meshes.SelectedItem = Meshes.Items.OfType<MeshChoice>().FirstOrDefault(item => item.Shader == selected.Shader);
            updating = false;
        }
        RefreshLink();
        FitMesh();
    }

    private void RefreshLink()
    {
        int? shader = (Materials.SelectedItem as MaterialChoice)?.Shader;
        var link = links.FirstOrDefault(item => item.Shader == shader);
        LinkDetails.Text = link is null ? "No recognized diffuse reference for this material." :
            $"{link.Slot} · {link.Status}\n{link.Reason}\nKey {link.Key ?? "unavailable"}" +
            string.Concat(link.Candidates.Select(candidate => $"\n{Path.GetFileName(candidate.Source)} · {candidate.AssetId}"));
        ShowLinked.IsEnabled = link?.TextureId is not null;
        LoadTexture(link?.TextureId is string id ? catalog!.Get(id) : null);
    }

    private void LoadTexture(Asset? asset)
    {
        texture = asset;
        textureInfo = null;
        poolWriteAvailable = false;
        poolWriteLimitation = null;
        originalImage = replacementImage = null;
        TextureDetails.Text = "";
        try
        {
            if (asset is not null)
            {
                var (dds, info) = XboxAssets.ReadTexture(catalog!, asset);
                textureInfo = info;
                originalImage = Bitmap(DdsTextureReader.DecodeTexturePixels(dds));
                var stage = project!.Find(asset);
                if (stage is not null) replacementImage = Bitmap(DdsTextureReader.DecodeTexturePixels(stage.Dds));
                TextureDetails.Text = $"{info.SourceWidth} × {info.SourceHeight} · DXGI {info.SourceFormat}\n" +
                    $"{info.ExportedMips} of {info.SourceMips} mips available starting at mip {info.BaseMip}.";
                var source = catalog!.SourceOf(asset);
                TtpfTextureInfo? poolTexture = null;
                var writeLimitations = new List<string>();
                if (source.Format == "ttpf" && asset.Index is int textureIndex)
                {
                    poolTexture = TtpfReader.Inspect(source.Path).Textures.Single(item => item.Index == textureIndex);
                    poolWriteAvailable = poolTexture.SupportsNativeReplacement;
                    if (!poolWriteAvailable)
                        writeLimitations.Add(poolTexture.Limitation ??
                            $"The source has only {poolTexture.Mips} of {poolTexture.SourceMips} mip levels available.");
                }
                if (stage is not null)
                {
                    var differences = ReplacementProject.Compare(info, DdsTextureReader.Inspect(stage.Dds));
                    TextureDetails.Text += $"\n{stage.OriginalFileName}";
                    TextureDetails.Text += differences.Length == 0 ? "\nDimensions, format, and mips match." :
                        string.Concat(differences.Select(field => $"\n{field.Field} {field.Original} → {field.Replacement}"));
                    if (poolTexture is not null && differences.Length > 0)
                    {
                        poolWriteAvailable = false;
                        writeLimitations.Add("The replacement DDS must match the source dimensions, DXGI format, and full mip chain.");
                    }
                }
                if (writeLimitations.Count > 0) poolWriteLimitation = string.Join(" ", writeLimitations);
                if (poolWriteLimitation is not null)
                    TextureDetails.Text += $"\nExperimental TTPF copy unavailable: {poolWriteLimitation}";
            }
        }
        catch (Exception error) when (IsAssetError(error))
        {
            TextureDetails.Text = error.Message;
        }
        ExportTextureButton.IsEnabled = textureInfo is not null;
        ImportButton.IsEnabled = originalImage is not null;
        bool staged = asset is not null && project!.Find(asset) is not null;
        RevertButton.IsEnabled = staged;
        WritePoolButton.IsEnabled = staged && poolWriteAvailable;
        WritePoolButton.ToolTip = poolWriteLimitation ?? "Writes and verifies a separate TTPF copy.";
        ShowReplacement.IsEnabled = replacementImage is not null;
        if (replacementImage is null) ShowOriginal.IsChecked = true;
        RefreshPreview();
    }

    private void ViewLinkedTexture(object sender, RoutedEventArgs e)
    {
        if (texture is not { } linked) return;
        TypeFilter.SelectedIndex = 0;
        Search.Text = linked.Id;
        AssetList.SelectedItem = linked;
        AssetList.ScrollIntoView(linked);
    }

    private void PreviewModeChanged(object sender, RoutedEventArgs e)
    {
        if (IsLoaded) RefreshPreview();
    }

    private void RefreshPreview()
    {
        var image = ShowReplacement.IsChecked == true ? replacementImage : originalImage;
        var choice = model is not null ? Meshes.SelectedItem as MeshChoice : null;
        TextureImage.Source = choice is null ? image : null;
        TextureImage.Visibility = choice is null && image is not null ? Visibility.Visible : Visibility.Collapsed;
        TextureSwatch.Source = choice is not null ? image : null;
        Viewport.Visibility = choice is null ? Visibility.Collapsed : Visibility.Visible;
        EmptyPreview.Visibility = choice is null && image is null ? Visibility.Visible : Visibility.Collapsed;
        ModelScene.Content = choice is null ? null : MeshScene(model!.Meshes[choice.Index], image);
    }

    private static Model3DGroup MeshScene(RageMesh mesh, ImageSource? image)
    {
        var geometry = new MeshGeometry3D
        {
            Positions = new(mesh.Positions.Select(point => new Point3D(point.X, point.Y, point.Z))),
            TriangleIndices = new(mesh.Indices)
        };
        Brush brush = new SolidColorBrush(Color.FromRgb(123, 144, 152));
        if (image is not null && mesh.TexCoords is { } uv && uv.Count == mesh.Positions.Count)
        {
            geometry.TextureCoordinates = new(uv.Select(point => new Point(point.X, point.Y)));
            brush = new ImageBrush(image) { TileMode = TileMode.Tile, Stretch = Stretch.Fill };
        }
        var material = new DiffuseMaterial(brush);
        return new Model3DGroup
        {
            Children =
            {
                new AmbientLight(Color.FromRgb(185, 185, 185)),
                new DirectionalLight(Color.FromRgb(140, 140, 140), new(-1, -2, -3)),
                new GeometryModel3D(geometry, material) { BackMaterial = material }
            }
        };
    }

    private void FitMesh()
    {
        if (model is null || Meshes.SelectedItem is not MeshChoice choice) return;
        var bounds = model.Meshes[choice.Index].Positions.Aggregate(Rect3D.Empty, (rect, p) =>
        {
            rect.Union(new Point3D(p.X, p.Y, p.Z));
            return rect;
        });
        if (bounds.IsEmpty) return;
        center = new(bounds.X + bounds.SizeX / 2, bounds.Y + bounds.SizeY / 2, bounds.Z + bounds.SizeZ / 2);
        distance = Math.Max(0.0001, new Vector3D(bounds.SizeX, bounds.SizeY, bounds.SizeZ).Length * 2);
        yaw = -1.05;
        pitch = 0.34;
        UpdateCamera();
    }

    private void UpdateCamera()
    {
        var delta = new Vector3D(Math.Cos(yaw) * Math.Cos(pitch), Math.Sin(yaw) * Math.Cos(pitch), Math.Sin(pitch)) * distance;
        Camera.Position = center + delta;
        Camera.LookDirection = -delta;
        Camera.UpDirection = new(0, 0, 1);
        Camera.NearPlaneDistance = distance / 300;
        Camera.FarPlaneDistance = distance * 50;
    }

    private void OrbitStart(object sender, MouseButtonEventArgs e)
    {
        drag = e.GetPosition(Viewport);
        Viewport.CaptureMouse();
    }

    private void OrbitEnd(object sender, MouseButtonEventArgs e)
    {
        drag = null;
        Viewport.ReleaseMouseCapture();
    }

    private void OrbitMove(object sender, MouseEventArgs e)
    {
        if (drag is not Point before || e.LeftButton != MouseButtonState.Pressed) return;
        Point now = e.GetPosition(Viewport);
        yaw -= (now.X - before.X) * 0.01;
        pitch = Math.Clamp(pitch + (now.Y - before.Y) * 0.01, -1.50, 1.50);
        drag = now;
        UpdateCamera();
    }

    private void Zoom(object sender, MouseWheelEventArgs e)
    {
        distance = Math.Clamp(distance * Math.Pow(1.18, -e.Delta / 120.0), 0.000001, 1e9);
        UpdateCamera();
    }

    private async void ExportAsset(object sender, RoutedEventArgs e)
    {
        if (catalog is null || selectedAsset is not { } asset) return;
        var dialog = new SaveFileDialog { Filter = asset.Type == "model" ? "Wavefront mesh|*.obj" : "Stored bytes|*.bin" };
        if (dialog.ShowDialog(this) != true) return;
        await Run("Exporting…", async () =>
        {
            await Task.Run(() =>
            {
                if (asset.Type == "model") XboxAssets.Export(catalog, asset, dialog.FileName);
                else LocalFiles.Write(dialog.FileName, stream => catalog.CopyStored(asset, stream));
            });
            StatusLine.Text = $"Exported {Path.GetFileName(dialog.FileName)}.";
        });
    }

    private async void ExportTexture(object sender, RoutedEventArgs e)
    {
        if (catalog is null || texture is not { } asset) return;
        var dialog = new SaveFileDialog { Filter = "DirectDraw surface|*.dds|PNG preview|*.png" };
        if (dialog.ShowDialog(this) != true) return;
        await Run("Exporting texture…", async () =>
        {
            if (dialog.FilterIndex == 2)
            {
                var preview = originalImage ?? throw new InvalidDataException("No readable preview for this texture.");
                var encoder = new PngBitmapEncoder();
                encoder.Frames.Add(BitmapFrame.Create(preview));
                LocalFiles.Write(dialog.FileName, encoder.Save);
            }
            else await Task.Run(() => XboxAssets.Export(catalog, asset, dialog.FileName));
            StatusLine.Text = $"Exported {Path.GetFileName(dialog.FileName)}.";
        });
    }

    private async void ImportReplacement(object sender, RoutedEventArgs e)
    {
        if (catalog is null || project is null || texture is not { } asset) return;
        var dialog = new OpenFileDialog { Filter = "Replacement textures|*.dds;*.png|DDS|*.dds|PNG|*.png" };
        if (dialog.ShowDialog(this) != true) return;
        await Run("Importing texture…", async () =>
        {
            await StageTextureFile(dialog.FileName, asset);
        });
    }

    private async Task StageTextureFile(string path, Asset asset)
    {
        await Task.Run(() => project!.Stage(catalog!, asset, path, ReadReplacement(path)));
        dirty = true;
        LoadTexture(asset);
        ShowReplacement.IsChecked = true;
        StatusLine.Text = "Replacement staged. Save the project to keep it.";
    }

    private void RevertReplacement(object sender, RoutedEventArgs e)
    {
        if (texture is not { } asset || project?.Revert(asset) != true) return;
        dirty = true;
        LoadTexture(asset);
        StatusLine.Text = "Original texture restored. Save the project to keep the revert.";
    }

    private async void WritePoolCopy(object sender, RoutedEventArgs e)
    {
        if (catalog is null || texture is not { } asset || project?.Find(asset) is not { } stage) return;
        var dialog = new SaveFileDialog { Filter = "Texture pool copy|*.ttpf", OverwritePrompt = true };
        if (dialog.ShowDialog(this) != true) return;
        var source = catalog.SourceOf(asset);
        await Run("Writing and reopening the TTPF copy…", async () =>
        {
            var result = await Task.Run(() => PoolWriter.Copy(catalog, asset, stage.Dds, dialog.FileName,
                overwriteExisting: true));
            if (result.ReplacedSource)
            {
                await Task.Run(() => catalog.Save(project!.CatalogPath ?? TemporaryCatalog, replace: true));
                project.RebaseSource(catalog, source.Id);
                dirty = true;
                StatusLine.Text = "Source TTPF replaced and catalog refreshed. Save the project to keep it; in-game compatibility is pending.";
            }
            else StatusLine.Text = "Copy created and verified with the texture reader. In-game compatibility is pending.";
        }, showErrors: true);
    }

    private static BitmapSource Bitmap(RageTexturePixels pixels) =>
        BitmapSource.Create(pixels.Width, pixels.Height, 96, 96, PixelFormats.Bgra32, null, pixels.Bgra32, pixels.Width * 4);

    private static byte[] ReadReplacement(string path)
    {
        const int maxBytes = 256 * 1024 * 1024;
        using var file = File.OpenRead(path);
        if (file.Length > maxBytes) throw new InvalidDataException("The replacement exceeds 256 MiB.");
        if (Path.GetExtension(path).Equals(".dds", StringComparison.OrdinalIgnoreCase))
        {
            var dds = new byte[(int)file.Length];
            file.ReadExactly(dds);
            return dds;
        }
        var frame = new PngBitmapDecoder(file, BitmapCreateOptions.PreservePixelFormat, BitmapCacheOption.OnDemand).Frames[0];
        if ((long)frame.PixelWidth * frame.PixelHeight > 16 * 1024 * 1024)
            throw new NotSupportedException("PNG replacements support up to 16 megapixels.");
        var bitmap = new FormatConvertedBitmap(frame, PixelFormats.Bgra32, null, 0);
        int stride = checked(bitmap.PixelWidth * 4);
        byte[] bytes = new byte[checked(stride * bitmap.PixelHeight)];
        bitmap.CopyPixels(bytes, stride, 0);
        return XboxAssets.CreatePreviewDds(new(bitmap.PixelWidth, bitmap.PixelHeight, bytes));
    }

    private async Task Run(string message, Func<Task> work, bool showErrors = false)
    {
        if (busy) return;
        busy = true;
        if (Content is UIElement workspace) workspace.IsEnabled = false;
        StatusLine.Text = message;
        try { await work(); }
        catch (Exception error) when (IsAssetError(error))
        {
            StatusLine.Text = error.Message;
            if (showErrors) MessageBox.Show(this, error.Message, "TTPF copy not written", MessageBoxButton.OK, MessageBoxImage.Warning);
        }
        finally
        {
            if (Content is UIElement workSurface) workSurface.IsEnabled = true;
            busy = false;
            if (closeAfterOpening)
            {
                closeAfterOpening = false;
                _ = Dispatcher.BeginInvoke(new Action(Close));
            }
        }
    }

    private static bool IsAssetError(Exception error) => error is IOException or InvalidDataException or
        UnauthorizedAccessException or ArgumentException or InvalidOperationException or NotSupportedException
        or System.Text.Json.JsonException or FormatException or OverflowException or KeyNotFoundException;

    private bool CanDiscard() => !dirty || MessageBox.Show(this, "Discard unsaved project changes?",
        "ViceModVI", MessageBoxButton.YesNo, MessageBoxImage.Question, MessageBoxResult.No) == MessageBoxResult.Yes;

    private void CloseWindow(object? sender, CancelEventArgs e)
    {
        if (!busy)
        {
            e.Cancel = !CanDiscard();
            return;
        }

        e.Cancel = true;
        if (openingCancellation is not { } cancellation || closeAfterOpening) return;
        closeAfterOpening = true;
        StatusLine.Text = "Canceling the current read…";
        cancellation.Cancel();
    }
}
