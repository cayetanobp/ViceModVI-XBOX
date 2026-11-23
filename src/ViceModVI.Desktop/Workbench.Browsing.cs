using System.Globalization;
using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media.Imaging;
using Microsoft.Win32;
using RpfPreview.Core;

namespace ViceModVI.Desktop;

public partial class Workbench
{
    private sealed record SourceScope(string Path, string DisplayPath)
    {
        public HashSet<string> SourceIds { get; } = new(StringComparer.Ordinal);
    }

    private sealed record AssetRow(Asset Asset, string SourcePath, string Location)
    {
        public string Name => Asset.Name;
        public string Summary => $"{Asset.Type} · {Location}";
    }

    private readonly Dictionary<string, TreeViewItem> sourceNodes = new(StringComparer.Ordinal);
    private HashSet<string> visibleSources = [];
    private AssetRow[] rows = [];

    private void BuildSourceTree(Catalog loaded, string? folder)
    {
        SourceTree.Items.Clear();
        sourceNodes.Clear();
        string? root = folder ?? CommonFolder(loaded.Sources);
        string title = root is null ? "All sources" : Path.GetFileName(Path.TrimEndingDirectorySeparator(root));
        if (title.Length == 0) title = root!;
        var rootNode = Node(title, root ?? "All sources", title);
        var folders = new Dictionary<string, TreeViewItem>(StringComparer.OrdinalIgnoreCase);
        if (root is not null) folders.Add(root, rootNode);
        var locations = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var source in loaded.Sources.OrderBy(item => item.Path, StringComparer.OrdinalIgnoreCase))
        {
            var owner = rootNode;
            Scope(owner).SourceIds.Add(source.Id);
            string location = root is null ? source.Path : Path.GetRelativePath(root, source.Path);
            locations.Add(source.Id, location);
            if (root is not null)
            {
                string directory = root;
                foreach (string part in location.Split(Path.DirectorySeparatorChar)[..^1])
                {
                    directory = Path.Combine(directory, part);
                    if (!folders.TryGetValue(directory, out var child))
                    {
                        child = Node(part, directory, Path.GetRelativePath(root, directory));
                        folders.Add(directory, child);
                        owner.Items.Add(child);
                    }
                    owner = child;
                    Scope(owner).SourceIds.Add(source.Id);
                }
            }
            var file = Node(root is null ? source.Path : Path.GetFileName(source.Path), source.Path, location);
            Scope(file).SourceIds.Add(source.Id);
            owner.Items.Add(file);
            sourceNodes.Add(source.Id, file);
        }
        var paths = loaded.Sources.ToDictionary(source => source.Id, source => source.Path);
        rows = loaded.Assets.Select(asset => new AssetRow(asset, paths[asset.SourceId], locations[asset.SourceId])).ToArray();
        SourceTree.Items.Add(rootNode);
        rootNode.IsExpanded = true;
        var initialNode = folder is null && sourceNodes.Count == 1 ? sourceNodes.Values.Single() : rootNode;
        initialNode.IsSelected = true;
        SetScope(Scope(initialNode));
    }

    private static string? CommonFolder(IReadOnlyList<Source> sources)
    {
        if (sources.Count == 0) return null;
        for (string? root = Path.GetDirectoryName(sources[0].Path); root is not null; root = Path.GetDirectoryName(root))
        {
            string prefix = Path.EndsInDirectorySeparator(root) ? root : root + Path.DirectorySeparatorChar;
            if (sources.All(source => source.Path.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))) return root;
        }
        return null;
    }

    private static TreeViewItem Node(string title, string path, string displayPath) => new()
    {
        Header = title,
        ToolTip = path,
        Tag = new SourceScope(path, displayPath)
    };

    private static SourceScope Scope(TreeViewItem item) => (SourceScope)item.Tag;

    private void SelectSource(object sender, RoutedPropertyChangedEventArgs<object> e)
    {
        if (updating || e.NewValue is not TreeViewItem node) return;
        SetScope(Scope(node));
        FilterChanged(this, new RoutedEventArgs());
    }

    private void SetScope(SourceScope scope)
    {
        visibleSources = scope.SourceIds;
        ScopePath.Text = scope.DisplayPath;
        ScopePath.ToolTip = scope.Path;
    }

    private void FilterChanged(object sender, RoutedEventArgs e)
    {
        if (catalog is null || updating) return;
        string query = Search.Text.Trim();
        var type = (TypeFilter.SelectedItem as ComboBoxItem)?.Tag as string;
        bool parsed = ulong.TryParse(query.StartsWith("0x", StringComparison.OrdinalIgnoreCase) ? query[2..] : query,
            NumberStyles.AllowHexSpecifier, CultureInfo.InvariantCulture, out ulong value);
        string? key = parsed ? value.ToString("X16") : null;
        var shown = rows.Where(row => visibleSources.Contains(row.Asset.SourceId) &&
            (type is null || row.Asset.Type == type) &&
            (query.Length == 0 || row.Name.Contains(query, StringComparison.OrdinalIgnoreCase) ||
             row.Asset.Id.Contains(query, StringComparison.OrdinalIgnoreCase) ||
             row.SourcePath.Contains(query, StringComparison.OrdinalIgnoreCase) ||
             string.Equals(row.Asset.Type, query, StringComparison.OrdinalIgnoreCase) ||
             (key is not null && (row.Asset.NativeKey == key || row.Asset.NameHash == key ||
                                  (value <= long.MaxValue && row.Asset.Range?.Offset == (long)value))))).ToArray();
        var selectedIds = AssetList.SelectedItems.OfType<AssetRow>().Select(row => row.Asset.Id).ToHashSet();
        updating = true;
        AssetList.ItemsSource = shown;
        foreach (var row in shown.Where(row => selectedIds.Contains(row.Asset.Id))) AssetList.SelectedItems.Add(row);
        updating = false;
        if ((AssetList.SelectedItem as AssetRow)?.Asset != selectedAsset)
            LoadAsset((AssetList.SelectedItem as AssetRow)?.Asset);
    }

    private void SelectAsset(object sender, SelectionChangedEventArgs e)
    {
        var asset = (AssetList.SelectedItem as AssetRow)?.Asset;
        if (!updating && asset != selectedAsset) LoadAsset(asset);
    }

    private void ViewLinkedTexture(object sender, RoutedEventArgs e)
    {
        if (texture is not { } linked || !sourceNodes.TryGetValue(linked.SourceId, out var node)) return;
        updating = true;
        for (var parent = ItemsControl.ItemsControlFromItemContainer(node) as TreeViewItem; parent is not null;
            parent = ItemsControl.ItemsControlFromItemContainer(parent) as TreeViewItem)
            parent.IsExpanded = true;
        node.IsSelected = true;
        SetScope(Scope(node));
        TypeFilter.SelectedIndex = 0;
        Search.Clear();
        updating = false;
        FilterChanged(this, new RoutedEventArgs());
        var row = rows.Single(item => item.Asset.Id == linked.Id);
        AssetList.SelectedItem = row;
        AssetList.ScrollIntoView(row);
        node.BringIntoView();
    }

    private void SelectContextAsset(object sender, MouseButtonEventArgs e)
    {
        if (e.OriginalSource is DependencyObject origin &&
            ItemsControl.ContainerFromElement(AssetList, origin) is ListBoxItem { IsSelected: false } item)
            AssetList.SelectedItem = item.Content;
    }

    private void PrepareExportMenu(object sender, RoutedEventArgs e)
    {
        var items = ((ContextMenu)sender).Items;
        var menu = (MenuItem)items[0];
        int count = AssetList.SelectedItems.Count;
        menu.Header = count > 1 ? $"Export {count} selected assets…" : "Export selection…";
        menu.IsEnabled = count > 0 && AssetList.SelectedItems.OfType<AssetRow>().All(row => CanExport(row.Asset));
        ((MenuItem)items[1]).Visibility = count == 1 && selectedAsset is { } asset && project?.Find(asset) is not null
            ? Visibility.Visible : Visibility.Collapsed;
    }

    private static bool CanExport(Asset asset) => asset.Type is "texture" or "model" || asset.Range is not null;

    private async void ExportStaged(object sender, RoutedEventArgs e)
    {
        if (selectedAsset is not { } asset || project?.Find(asset) is not { } staged) return;
        var dialog = new SaveFileDialog { Filter = "DirectDraw surface|*.dds", OverwritePrompt = false,
            FileName = Path.GetFileNameWithoutExtension(staged.OriginalFileName) + "-staged" };
        if (dialog.ShowDialog(this) != true) return;
        await Run("Exporting staged DDS…", async () =>
        {
            await Task.Run(() => WriteExport(catalog!, asset, dialog.FileName, staged: staged));
            StatusLine.Text = $"Exported {Path.GetFileName(dialog.FileName)}.";
        });
    }

    private async void ExportSelection(object sender, RoutedEventArgs e)
    {
        if (catalog is null) return;
        var selection = AssetList.SelectedItems.OfType<AssetRow>().Select(row => row.Asset).ToArray();
        if (selection.Length == 0 || selection.Any(item => !CanExport(item))) return;
        if (selection is [var asset])
        {
            await ExportOne(asset);
            return;
        }

        var folder = new OpenFolderDialog { Title = "Choose a folder for the selected exports" };
        if (folder.ShowDialog(this) != true) return;
        await Run($"Exporting {selection.Length} assets…", async () =>
        {
            var (exported, failures) = await Task.Run(() => ExportBatch(catalog, selection, folder.FolderName));
            StatusLine.Text = $"Exported {exported} of {selection.Length} assets.";
            if (failures.Count > 0)
                MessageBox.Show(this, string.Join("\n", failures.Take(8)), "Exports needing attention",
                    MessageBoxButton.OK, MessageBoxImage.Information);
        });
    }

    private static (int Exported, List<string> Failures) ExportBatch(Catalog catalog, Asset[] selection, string directory)
    {
        int exported = 0;
        var failures = new List<string>();
        foreach (var asset in selection)
        {
            string path = Path.Combine(directory, ExportName(asset) + ExportExtension(asset));
            try
            {
                WriteExport(catalog, asset, path);
                exported++;
            }
            catch (Exception error) when (IsAssetError(error))
            {
                failures.Add($"{asset.Name} — {error.Message}");
            }
        }
        return (exported, failures);
    }

    private async void ExportPreviewTexture(object sender, RoutedEventArgs e)
    {
        if (texture is not { } asset) return;
        var staged = ShowReplacement.IsChecked == true ? project?.Find(asset) : null;
        await ExportOne(asset, staged);
    }

    private async Task ExportOne(Asset asset, StagedTexture? staged = null)
    {
        string filter = asset.Type switch
        {
            "model" => "Wavefront mesh|*.obj",
            "texture" => "DirectDraw surface|*.dds|PNG preview|*.png",
            _ => "Stored bytes|*.bin"
        };
        string fileName = staged is null ? ExportName(asset) : Path.GetFileNameWithoutExtension(staged.OriginalFileName) + "-staged";
        var dialog = new SaveFileDialog { Filter = filter, FileName = fileName, OverwritePrompt = false };
        if (dialog.ShowDialog(this) != true) return;
        bool png = asset.Type == "texture" && dialog.FilterIndex == 2;
        await Run("Exporting…", async () =>
        {
            await Task.Run(() => WriteExport(catalog!, asset, dialog.FileName, png, staged));
            StatusLine.Text = $"Exported {Path.GetFileName(dialog.FileName)}.";
        });
    }

    private static string ExportName(Asset asset)
    {
        var invalid = Path.GetInvalidFileNameChars();
        string clean = new(asset.Name.Select(ch => ch == '.' || invalid.Contains(ch) ? '_' : ch).Take(80).ToArray());
        string identity = new(asset.Id.Select(ch => invalid.Contains(ch) ? '_' : ch).ToArray());
        return $"{clean}_{identity}";
    }

    private static string ExportExtension(Asset asset) => asset.Type switch
    {
        "model" => ".obj",
        "texture" => ".dds",
        _ => ".bin"
    };

    private static void WriteExport(Catalog catalog, Asset asset, string path, bool png = false, StagedTexture? staged = null)
    {
        if (png)
        {
            byte[] dds = staged?.Dds ?? XboxAssets.ReadTexture(catalog, asset).Dds;
            var encoder = new PngBitmapEncoder();
            encoder.Frames.Add(BitmapFrame.Create(Bitmap(DdsTextureReader.DecodeTexturePixels(dds))));
            LocalFiles.Write(path, encoder.Save);
        }
        else if (staged is not null) LocalFiles.Write(path, stream => stream.Write(staged.Dds));
        else if (asset.Type is "texture" or "model") XboxAssets.Export(catalog, asset, path);
        else LocalFiles.Write(path, stream => catalog.CopyStored(asset, stream));
    }
}
