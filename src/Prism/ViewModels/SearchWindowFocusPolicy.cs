namespace Prism.ViewModels;

public static class SearchWindowFocusPolicy
{
    public static bool ShouldHide(
        bool ignoreDeactivate,
        bool contextMenuOpen,
        bool contextMenuActionPending,
        bool isPinned,
        bool isHiding,
        bool isDragging = false) =>
        !(ignoreDeactivate || contextMenuOpen || contextMenuActionPending
          || isPinned || isHiding || isDragging);
}
