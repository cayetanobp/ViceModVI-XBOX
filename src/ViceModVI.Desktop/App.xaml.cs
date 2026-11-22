using System.Windows;

namespace ViceModVI.Desktop;

public partial class App : Application
{
    protected override async void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        var window = new Workbench();
        MainWindow = window;
        window.Show();
        if (e.Args.Length == 1) await window.OpenPath(e.Args[0]);
    }
}
