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
    private Asset? selectedAsset;
    private Asset? texture;
    private TextureExport? textureInfo;
    private RageModel? model;
    private TextureLink[] links = [];
    private BitmapSource? originalImage;
    private BitmapSource? replacementImage;
    private bool updating;
    private bool busy;
    private bool closeAfterOpening;
    private CancellationTokenSource? openingCancellation;
    private CancellationTokenSource? textureCancellation;
    private TextureConversionPlan? poolConversion;
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
            if (Path.GetExtension(path).Equals(".json", StringComparison.OrdinalIgnoreCase) && File.Exists(path))
            {
                var loaded = await Task.Run(() => Catalog.Load(path));
                SetWorkspace(loaded);
            }
            else await OpenSources(path);
        });
    }

    private void SetWorkspace(Catalog loaded, string? folder = null)
    {
        catalog = loaded;
        project = new();
        selectedAsset = null;
        updating = true;
        Search.Clear();
        TypeFilter.SelectedIndex = 0;
        BuildSourceTree(loaded, folder);
        updating = false;
        FilterChanged(this, new RoutedEventArgs());
        LoadAsset(null);
        int ranges = loaded.Assets.Count(asset => asset.Type == "unknown");
        int models = loaded.Assets.Count(asset => asset.Type == "model");
        StatusLine.Text = $"{models:N0} models · " +
            $"{loaded.Assets.Count - ranges:N0} identified entries and {ranges:N0} unknown ranges.";
    }

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
            bool isFolder = Directory.Exists(path);
            var loaded = await Task.Run(() =>
            {
                IEnumerable<string> paths = isFolder
                    ? Directory.EnumerateFiles(path, "*", SearchOption.AllDirectories)
                        .Where(p => Path.GetExtension(p).ToLowerInvariant() is ".rpf" or ".ttpf")
                        .Order(StringComparer.OrdinalIgnoreCase)
                    : [path];
                return XboxAssets.Build(paths, progress: (current, entry) => progress.Report((current, entry)),
                    cancellationToken: cancellation.Token);
            });
            if (!cancellation.IsCancellationRequested) SetWorkspace(loaded, isFolder ? path : null);
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

    private void LoadAsset(Asset? asset)
    {
        if (catalog is null) return;
        selectedAsset = asset;
        model = null;
        links = [];
        MeshControls.Visibility = Visibility.Collapsed;
        DictionaryControls.Visibility = Visibility.Collapsed;
        DictionaryTextures.ItemsSource = null;
        AssetHeading.Text = asset?.Name ?? "Preview";
        string? sourcePath = asset is null ? null : catalog.SourceOf(asset).Path;
        AssetDetails.Text = asset is null ? "" : $"{Path.GetFileName(sourcePath)}\n{asset.Id} · {asset.NameOrigin}";
        AssetDetails.ToolTip = asset is null ? null : $"{sourcePath}\n{asset.Evidence}";
        _ = LoadTextureAsync(null);
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
            else if (asset.Type == "texture") _ = LoadTextureAsync(asset);
            else if (asset.Type == "dictionary")
            {
                var children = catalog.Assets.Where(item => item.Type == "texture" && item.ParentId == asset.Id)
                    .OrderBy(item => item.Index).ToArray();
                if (children.Length == 0) return;
                DictionaryControls.Visibility = Visibility.Visible;
                DictionaryTextures.ItemsSource = children;
                DictionaryTextures.SelectedIndex = 0;
            }
        }
        catch (Exception error) when (IsAssetError(error))
        {
            StatusLine.Text = error.Message;
        }
    }

    private void SelectDictionaryTexture(object sender, SelectionChangedEventArgs e)
    {
        if (DictionaryTextures.SelectedItem is Asset asset) _ = LoadTextureAsync(asset);
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
        LinkDetails.Text = link is null ? "No recognized diffuse reference." :
            $"{link.Slot} · {link.Status}" + (link.TextureId is not null ? $"\n{Path.GetFileName(link.Candidates[0].Source)}" :
                $"\n{link.Reason}" + string.Concat(link.Candidates.Select(candidate =>
                    $"\n{Path.GetFileName(candidate.Source)} · {candidate.AssetId}")));
        LinkDetails.ToolTip = link is null ? null : $"{link.Reason}\n{link.Key ?? "Key unavailable"}" +
            string.Concat(link.Candidates.Select(candidate => $"\n{candidate.Source} · {candidate.AssetId}"));
        ShowLinked.Visibility = link?.TextureId is not null ? Visibility.Visible : Visibility.Collapsed;
        _ = LoadTextureAsync(link?.TextureId is string id ? catalog!.Get(id) : null);
    }

    private async Task LoadTextureAsync(Asset? asset)
    {
        textureCancellation?.Cancel();
        textureCancellation = null;
        texture = asset;
        textureInfo = null;
        poolConversion = null;
        originalImage = replacementImage = null;
        TextureDetails.Text = "";
        EditDetails.Text = "";
        var stage = asset is null ? null : project!.Find(asset);
        bool poolTexture = asset is not null && catalog!.SourceOf(asset).Format == "ttpf";
        if (asset is not null)
        {
            using var cancellation = new CancellationTokenSource();
            textureCancellation = cancellation;
            var sourceCatalog = catalog!;
            EditMenu.Visibility = CompareControls.Visibility = ExportTextureButton.Visibility = Visibility.Collapsed;
            if (model is not null) TextureDetails.Text = "Reading texture…";
            RefreshPreview();
            try
            {
                var (pixels, info) = await Task.Run(() =>
                {
                    var (dds, export) = XboxAssets.ReadTexture(sourceCatalog, asset, cancellation.Token);
                    return (DdsTextureReader.DecodeTexturePixels(dds), export);
                }, cancellation.Token);
                if (cancellation.IsCancellationRequested) return;
                textureInfo = info;
                originalImage = Bitmap(pixels);
                TextureDetails.Text = $"{info.SourceWidth} × {info.SourceHeight} · DXGI {info.SourceFormat}\n" +
                    $"{info.ExportedMips}/{info.SourceMips} mips · starts at {info.BaseMip}";
            }
            catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
            {
                return;
            }
            catch (Exception error) when (IsAssetError(error))
            {
                if (cancellation.IsCancellationRequested) return;
                TextureDetails.Text = error.Message;
            }
            finally
            {
                if (textureCancellation == cancellation) textureCancellation = null;
            }
            if (stage is not null)
            {
                var ddsInfo = DdsTextureReader.Inspect(stage.Dds);
                EditDetails.Text = $"{stage.OriginalFileName}\n{TextureShape.From(ddsInfo)}";
                try
                {
                    replacementImage = Bitmap(DdsTextureReader.DecodeTexturePixels(stage.Dds));
                }
                catch (Exception error) when (IsAssetError(error))
                {
                    EditDetails.Text += $"\nPreview unavailable. {error.Message}";
                }
                if (textureInfo is { } info && ReplacementProject.Compare(info, ddsInfo).Length != 0)
                    EditDetails.Text += "\nPreview can differ from the final material.";
                if (poolTexture) InspectPoolConversion(asset, stage);
            }
        }
        EditMenu.Visibility = ExportTextureButton.Visibility = asset is null ? Visibility.Collapsed : Visibility.Visible;
        EditDetails.Visibility = stage is null ? Visibility.Collapsed : Visibility.Visible;
        RevertAction.Visibility = stage is null ? Visibility.Collapsed : Visibility.Visible;
        ConvertAction.Visibility = stage is not null && poolTexture ? Visibility.Visible : Visibility.Collapsed;
        ConvertAction.IsEnabled = poolConversion is not null;
        ShowOriginal.IsEnabled = originalImage is not null;
        ShowReplacement.IsEnabled = replacementImage is not null;
        CompareControls.Visibility = replacementImage is null ? Visibility.Collapsed : Visibility.Visible;
        if (replacementImage is null) ShowOriginal.IsChecked = true;
        else if (originalImage is null) ShowReplacement.IsChecked = true;
        RefreshPreview();
    }

    private void InspectPoolConversion(Asset asset, StagedTexture stage)
    {
        try
        {
            poolConversion = PoolWriter.Plan(catalog!, asset, stage.Dds);
            if (poolConversion.Limitation is { } limitation) EditDetails.Text += $"\n{limitation}";
        }
        catch (Exception error) when (IsAssetError(error))
        {
            EditDetails.Text += $"\n{error.Message}";
        }
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
        Viewport.Visibility = choice is null ? Visibility.Collapsed : Visibility.Visible;
        EmptyPreview.Visibility = choice is null && image is null ? Visibility.Visible : Visibility.Collapsed;
        if (EmptyPreview.Visibility == Visibility.Visible)
            EmptyPreview.Text = selectedAsset?.Type switch
            {
                "texture" or "dictionary" when textureCancellation is not null => "Reading texture…",
                "dictionary" when texture is null => "No textures were indexed for this dictionary.",
                "texture" or "dictionary" => "Texture preview unavailable. See the inspector for details.",
                "model" => "Model preview unavailable.",
                null when catalog is null => "Open an archive or folder to get started.",
                null => "Select a model or texture.",
                _ => "No preview is available for this entry."
            };
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

    private async void ImportReplacement(object sender, RoutedEventArgs e)
    {
        if (catalog is null || project is null || texture is not { } asset) return;
        var dialog = new OpenFileDialog { Filter = TextureConversion.FileFilter };
        if (dialog.ShowDialog(this) != true) return;
        await Run("Importing texture…", async () =>
        {
            await StageTextureFile(dialog.FileName, asset);
        });
    }

    private async Task StageTextureFile(string path, Asset asset)
    {
        await Task.Run(() => project!.Stage(catalog!, asset, path, TextureConversion.Import(path)));
        await LoadTextureAsync(asset);
        if (replacementImage is not null) ShowReplacement.IsChecked = true;
        StatusLine.Text = "Replacement staged for this session.";
    }

    private async void RevertReplacement(object sender, RoutedEventArgs e)
    {
        if (texture is not { } asset || project?.Revert(asset) != true) return;
        await LoadTextureAsync(asset);
        if (texture == asset)
            StatusLine.Text = originalImage is null ? "Replacement cleared." : "Original texture restored.";
    }

    private async void ConvertForPool(object sender, RoutedEventArgs e)
    {
        if (catalog is null || texture is not { } asset || project?.Find(asset) is not { } stage) return;
        var plan = poolConversion;
        if (plan is null) return;
        string details = $"Imported DDS\n{plan.Input}\n\nExport DDS\n{plan.Output}";
        if (plan.Limitation is { } limitation)
        {
            MessageBox.Show(this, $"{details}\n\n{limitation}\nThe imported DDS remains staged.",
                "Convert for TTPF export", MessageBoxButton.OK, MessageBoxImage.Information);
            return;
        }
        string warnings = plan.Warnings.Count == 0 ? "" : $"\n\n{string.Join("\n", plan.Warnings)}";
        string consent = plan.RequiresResize ? "Resize the export copy to the pool dimensions?" : "Export this DDS?";
        if (MessageBox.Show(this, $"{details}{warnings}\n\n{consent}",
                "Convert for TTPF export", MessageBoxButton.YesNo, MessageBoxImage.Question,
                MessageBoxResult.No) != MessageBoxResult.Yes) return;
        var dialog = new SaveFileDialog { Title = "Export compatible DDS", Filter = "DirectDraw surface|*.dds",
            FileName = Path.GetFileNameWithoutExtension(stage.OriginalFileName) + "-ttpf.dds", OverwritePrompt = false };
        if (dialog.ShowDialog(this) != true) return;
        await Run("Preparing DDS…", async () =>
        {
            await Task.Run(() =>
            {
                byte[] bytes = TextureConversion.Apply(stage.Dds, plan, allowResize: plan.RequiresResize);
                LocalFiles.Write(dialog.FileName, stream => stream.Write(bytes));
            });
            StatusLine.Text = $"Exported {Path.GetFileName(dialog.FileName)}. The staged preview remains available.";
        });
    }

    private static BitmapSource Bitmap(RageTexturePixels pixels) =>
        BitmapSource.Create(pixels.Width, pixels.Height, 96, 96, PixelFormats.Bgra32, null, pixels.Bgra32, pixels.Width * 4);

    private async Task Run(string message, Func<Task> work)
    {
        if (busy) return;
        busy = true;
        if (Content is UIElement workspace) workspace.IsEnabled = false;
        StatusLine.Text = message;
        try { await work(); }
        catch (Exception error) when (IsAssetError(error))
        {
            StatusLine.Text = error.Message;
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

    private bool CanDiscard() => project?.Replacements.Count is null or 0 || MessageBox.Show(this, "Discard staged texture edits?",
        "ViceModVI", MessageBoxButton.YesNo, MessageBoxImage.Question, MessageBoxResult.No) == MessageBoxResult.Yes;

    private void CloseWindow(object? sender, CancelEventArgs e)
    {
        if (!busy)
        {
            e.Cancel = !CanDiscard();
            if (!e.Cancel) textureCancellation?.Cancel();
            return;
        }

        e.Cancel = true;
        if (openingCancellation is not { } cancellation || closeAfterOpening) return;
        closeAfterOpening = true;
        StatusLine.Text = "Canceling the current read…";
        cancellation.Cancel();
    }
}
