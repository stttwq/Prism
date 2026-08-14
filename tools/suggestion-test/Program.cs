using Prism.Services;

// 模拟真实运行环境：net8.0-windows, 无 settings.json, 默认引擎
var svc = new SuggestionService();

Console.WriteLine("=== Testing Baidu suggestion for '知乎' ===");
var result = await svc.GetSuggestionsAsync("百度", "知乎", CancellationToken.None);
Console.WriteLine($"Suggestion count: {result.Count}");
foreach (var s in result)
{
    Console.WriteLine($"  Text: [{s.Text}]  Url: {s.Url}");
}

Console.WriteLine();
Console.WriteLine("=== Testing Baidu suggestion for '知' ===");
var result2 = await svc.GetSuggestionsAsync("百度", "知", CancellationToken.None);
Console.WriteLine($"Suggestion count: {result2.Count}");
foreach (var s in result2)
{
    Console.WriteLine($"  Text: [{s.Text}]  Url: {s.Url}");
}
