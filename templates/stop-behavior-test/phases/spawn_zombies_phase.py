import subprocess


def spawn_zombies_phase(test_api, ui):
    """Spawn multiple child processes to test zombie cleanup"""
    print("[Spawn Zombies] Creating 3 long-running child processes")
    print("[Spawn Zombies] These should be cleaned up automatically when worker exits")

    children = []
    for i in range(3):
        proc = subprocess.Popen(["sleep", "300"])
        children.append(proc)
        print(f"[Spawn Zombies] Spawned child process {proc.pid}")

    print("[Spawn Zombies] Phase complete, children still running")
    print("[Spawn Zombies] Check with 'ps aux | grep sleep' to see them")
    print("[Spawn Zombies] They should be killed when worker is stopped")
