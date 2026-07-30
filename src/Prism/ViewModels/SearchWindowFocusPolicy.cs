namespace Prism.ViewModels;

public static class SearchWindowFocusPolicy
{
    public static bool ShouldHide(
        bool ignoreDeactivate,
        bool contextMenuOpen,
        bool contextMenuActionPending,
        bool isPinned,
        bool isHiding) =>
        !(ignoreDeactivate || contextMenuOpen || contextMenuActionPending || isPinned || isHiding);
}
