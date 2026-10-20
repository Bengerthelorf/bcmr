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
    version = subprocess.check_output(
        [zsh, "-f", "-c", 'print -r -- "$ZSH_VERSION"'], env=env, timeout=10,
        text=True).strip()
    output = subprocess.check_output(
        [zsh, "-f", "-c", 'printf "%s\\0" "${fpath[@]}"'], env=env, timeout=10)
    directories = [os.fsdecode(path) for path in output.split(b"\0") if path]
    # Vendor/site completions under /usr/share/zsh may belong to other packages
    # and symlink to insecure locations. Use only zsh's own function trees.
    roots = ("/usr/share/zsh/functions", f"/usr/share/zsh/{version}/functions")
    trusted = [path for path in directories
               if any(path == root or path.startswith(root + "/") for root in roots)
               and Path(path).is_dir()]
    if not any((Path(path) / "compinit").is_file() for path in trusted):
        raise RuntimeError("zsh distribution functions under /usr/share/zsh lack compinit")
    selected = os.pathsep.join(dict.fromkeys(trusted))
    env["FPATH"] = selected
    audit = subprocess.run([zsh, "-f", "-c", 'autoload -Uz compaudit; compaudit'],
                           env=env, capture_output=True, text=True, timeout=10)
    if audit.returncode:
        raise RuntimeError(f"distribution FPATH failed compaudit ({audit.returncode}): "
                           f"{selected}\n{audit.stdout}{audit.stderr}")
    return selected


def with_system_zsh_fpath(env):
    """Export an isolated FPATH for both subprocess and PTY zsh invocations."""
    isolated = dict(env)
    zsh = shutil.which("zsh")
    if zsh:
        isolated["FPATH"] = system_zsh_fpath(zsh)
    return isolated
