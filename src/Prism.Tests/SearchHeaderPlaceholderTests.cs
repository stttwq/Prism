using System.Windows.Threading;
using Prism.Controls;
using Prism.Models;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// 审计批次 3 U3：动作模式的占位文案。
/// 此前恒为"搜索应用和文件"，而该输入框在动作模式下筛选的是动作列表。
/// </summary>
public sealed class SearchHeaderPlaceholderTests
{
    [Fact]
    public void Placeholder_Follows_Panel_Mode()
    {
        var texts = new List<string>();
        RunOnSta(() =>
        {
            var header = new SearchHeader();
            header.SetMode(PanelMode.Actions);
            texts.Add(header.Placeholder.Text);
            header.SetMode(PanelMode.Results);
            texts.Add(header.Placeholder.Text);
        });

        Assert.Equal(["输入以筛选动作", "搜索应用和文件"], texts);
    }

    private static void RunOnSta(Action action)
    {
        Exception? error = null;
        var thread = new Thread(() =>
        {
            try { action(); }
            catch (Exception ex) { error = ex; }
            finally { Dispatcher.CurrentDispatcher.InvokeShutdown(); }
        });
        thread.SetApartmentState(ApartmentState.STA);
        thread.IsBackground = true;
        thread.Start();
        Assert.True(thread.Join(TimeSpan.FromSeconds(30)), "STA 线程超时");
        if (error is not null)
            throw new Xunit.Sdk.XunitException("STA 线程内失败：" + error);
    }
}
