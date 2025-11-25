import time


def blocking_sleep_phase(run, ui):
    """Long sleep without signal handling - will not respond to SIGTERM gracefully"""
    print("[Blocking Sleep] Starting 60 second sleep WITHOUT signal handling")
    print(
        "[Blocking Sleep] Try stopping this - it should be force killed after timeout"
    )
    time.sleep(60)
    print("[Blocking Sleep] Completed (this shouldn't print if stopped)")
