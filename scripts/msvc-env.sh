# Put the MSVC toolchain ahead of Git's coreutils on PATH.
#
# Why this exists: Git for Windows ships /usr/bin/link (a coreutils hardlink
# tool). When it wins the PATH race, rustc invokes it as the linker and fails
# with "link: extra operand ...". Library-only builds still succeed because
# they never link, so the breakage only shows up when producing an .exe.
#
# Usage:  source scripts/msvc-env.sh
#
# Discovers the toolset and SDK versions instead of pinning them, so a VS or
# SDK update does not silently reintroduce the old failure.

set -u

_vs_root="/c/Program Files (x86)/Microsoft Visual Studio/2022/BuildTools"
_kits_lib="/c/Program Files (x86)/Windows Kits/10/Lib"
_kits_inc="/c/Program Files (x86)/Windows Kits/10/Include"

if [ ! -d "$_vs_root" ]; then
  echo "msvc-env: BuildTools not found at $_vs_root" >&2
  echo "msvc-env: install with:" >&2
  echo '  winget install --id Microsoft.VisualStudio.2022.BuildTools --override "--quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"' >&2
  return 1 2>/dev/null || exit 1
fi

# Highest installed MSVC toolset.
_msvc_ver="$(ls "$_vs_root/VC/Tools/MSVC" 2>/dev/null | sort -V | tail -1)"
if [ -z "$_msvc_ver" ]; then
  echo "msvc-env: no MSVC toolset under $_vs_root/VC/Tools/MSVC" >&2
  return 1 2>/dev/null || exit 1
fi

# Highest SDK that actually carries the x64 import libs we link against.
_sdk_ver=""
for _candidate in $(ls "$_kits_lib" 2>/dev/null | sort -V -r); do
  if [ -f "$_kits_lib/$_candidate/um/x64/kernel32.lib" ]; then
    _sdk_ver="$_candidate"
    break
  fi
done
if [ -z "$_sdk_ver" ]; then
  echo "msvc-env: no Windows SDK with um/x64/kernel32.lib under $_kits_lib" >&2
  return 1 2>/dev/null || exit 1
fi

_msvc_bin="$_vs_root/VC/Tools/MSVC/$_msvc_ver/bin/Hostx64/x64"
PATH="$_msvc_bin:$PATH"
export PATH

# rustc passes these to link.exe; semicolon-separated Windows paths.
LIB="$(cygpath -w "$_vs_root/VC/Tools/MSVC/$_msvc_ver/lib/x64");$(cygpath -w "$_kits_lib/$_sdk_ver/ucrt/x64");$(cygpath -w "$_kits_lib/$_sdk_ver/um/x64")"
INCLUDE="$(cygpath -w "$_vs_root/VC/Tools/MSVC/$_msvc_ver/include");$(cygpath -w "$_kits_inc/$_sdk_ver/ucrt");$(cygpath -w "$_kits_inc/$_sdk_ver/um");$(cygpath -w "$_kits_inc/$_sdk_ver/shared")"
export LIB INCLUDE

# The .NET SDK installs to a fixed path that Git Bash does not pick up, so the
# C# half of the build fails with "dotnet: command not found" even when the SDK
# is present.
if [ -x "/c/Program Files/dotnet/dotnet.exe" ]; then
  case ":$PATH:" in
    *":/c/Program Files/dotnet:"*) ;;
    *) PATH="/c/Program Files/dotnet:$PATH"; export PATH ;;
  esac
fi

echo "msvc-env: MSVC $_msvc_ver, SDK $_sdk_ver"
echo "msvc-env: link -> $(command -v link)"
echo "msvc-env: dotnet -> $(command -v dotnet 2>/dev/null || echo 'NOT FOUND')"

unset _vs_root _kits_lib _kits_inc _msvc_ver _sdk_ver _msvc_bin _candidate
