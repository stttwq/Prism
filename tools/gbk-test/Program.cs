using System;
using System.Text;
using System.Net.Http;

var url = "https://suggestion.baidu.com/su?wd=%E7%9F%A5%E4%B9%8E&action=opensearch";
using var http = new HttpClient { Timeout = TimeSpan.FromSeconds(5) };
var resp = await http.GetAsync(url, HttpCompletionOption.ResponseHeadersRead);
var charset = resp.Content.Headers.ContentType?.CharSet;
Console.WriteLine($"Content-Type charset: {charset}");
var stream = await resp.Content.ReadAsStreamAsync();
var buf = new byte[4096];
var n = await stream.ReadAsync(buf);
Console.WriteLine($"Bytes read: {n}");
Console.Write("Hex: ");
for (int i = 0; i < Math.Min(n, 20); i++) Console.Write($"{buf[i]:X2} ");
Console.WriteLine();
Console.WriteLine($"UTF8: {Encoding.UTF8.GetString(buf, 0, n)}");
Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
try {
    var gbk = Encoding.GetEncoding("gbk");
    Console.WriteLine($"GBK:  {gbk.GetString(buf, 0, n)}");
} catch (Exception ex) {
    Console.WriteLine($"GBK failed: {ex.Message}");
}
