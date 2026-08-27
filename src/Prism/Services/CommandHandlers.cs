namespace Prism.Services;

/// <summary>
/// K1：UI handler 注册表。broker 回传 UiCommand 时，前端按 command id 查表执行。
/// K0 仅判定 IsKnown（过滤目录），K1 增加 TryExecute 执行本地 handler。
/// handler 接收 CommandInvocationContext（K1 暂不使用参数，预留 K2+ 参数化命令）。
/// </summary>
internal static class CommandHandlers
{
    public delegate void UiCommandHandler(Models.CommandInvocationContext context);

    private static readonly Dictionary<string, UiCommandHandler> Handlers = new(StringComparer.Ordinal)
    {
        ["prism.settings.open"] = OpenSettings,
    };

    /// <summary>id 是否在 UI handler 注册表内（目录过滤用）。</summary>
    public static bool IsKnown(string id) => Handlers.ContainsKey(id);

    /// <summary>
    /// K1：执行 UI-owned 命令。id 未注册返回 false（调用方据此报错）。
    /// handler 在 UI 线程执行（调用方已在 UI 线程）。
    /// </summary>
    public static bool TryExecute(string id, Models.CommandInvocationContext context)
    {
        if (!Handlers.TryGetValue(id, out var handler))
            return false;
        handler(context);
        return true;
    }

    private static void OpenSettings(Models.CommandInvocationContext context)
    {
        var app = System.Windows.Application.Current;
        if (app is null) return;
        app.Dispatcher.BeginInvoke(new Action(() =>
        {
            // App.OpenSettings 已改为 public（K1 需求）。
            ((App)app).OpenSettings();
        }));
    }
}
