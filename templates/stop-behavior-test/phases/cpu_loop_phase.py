def cpu_loop_phase(run, ui):
    """Tight CPU loop without signal checks - will not respond to SIGTERM"""
    print("[CPU Loop] Starting CPU-intensive calculation")
    print("[CPU Loop] Try stopping this - it won't respond until loop completes")
    result = 0
    for i in range(10**9):
        result += i * 2
    print(f"[CPU Loop] Completed with result={result}")
