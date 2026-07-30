using System.Windows.Threading;

namespace Prism.Services;

public sealed class DispatcherDebounceTimerFactory : IDebounceTimerFactory
{
    public IDebounceTimer Create(TimeSpan interval, Action callback) =>
        new DispatcherDebounceTimer(interval, callback);
}

internal sealed class DispatcherDebounceTimer : IDebounceTimer
{
    private readonly DispatcherTimer _timer;

    public DispatcherDebounceTimer(TimeSpan interval, Action callback)
    {
        _timer = new DispatcherTimer { Interval = interval };
        _timer.Tick += (_, _) =>
        {
            _timer.Stop();
            callback();
        };
    }

    public void Restart()
    {
        _timer.Stop();
        _timer.Start();
    }

    public void Stop() => _timer.Stop();
}
