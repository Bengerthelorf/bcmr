"""Use distribution zsh functions without trusting machine-specific additions."""
import functools
import os
from pathlib import Path
import shutil
import subprocess


@functools.lru_cache(maxsize=None)
def system_zsh_fpath(zsh):
    # Discover the shell's compiled/system defaults, not the caller's exported
    # Homebrew, plugin-manager, or CI site-functions paths.
    env = dict(os.environ)
    env.pop("FPATH", None)
    output = subprocess.check_output(
        [zsh, "-f", "-c", 'printf "%s\\0" "${fpath[@]}"'], env=env, timeout=10)
    directories = [os.fsdecode(path) for path in output.split(b"\0") if path]
    trusted = [path for path in directories
               if path.startswith("/usr/share/zsh/") and Path(path).is_dir()]
    if not any((Path(path) / "compinit").is_file() for path in trusted):
        raise RuntimeError("zsh distribution functions under /usr/share/zsh lack compinit")
    return os.pathsep.join(dict.fromkeys(trusted))


def with_system_zsh_fpath(env):
    """Export an isolated FPATH for both subprocess and PTY zsh invocations."""
    isolated = dict(env)
    zsh = shutil.which("zsh")
    if zsh:
        isolated["FPATH"] = system_zsh_fpath(zsh)
    return isolated
