import subprocess


def subprocess_blocking_phase(test_api, ui):
    """Spawned subprocess - parent blocks while subprocess runs"""
    print("[Subprocess] Spawning 'sleep 60' subprocess")
    print("[Subprocess] Try stopping this - subprocess.run() blocks parent")
    try:
        subprocess.run(["sleep", "60"], check=True)
        print("[Subprocess] Subprocess completed")
    except Exception as e:
        print(f"[Subprocess] Error: {e}")
