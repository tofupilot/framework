"""Test RETRY phase return value cases."""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def return_phase_result_retry(phase):
    """Call phase.RETRY → Retry (stops before reaching limit)."""
    memory_size = (run.retry_count + 1) * 15 * 1024 * 1024
    data = bytearray(memory_size)

    sleep_times = [0.3, 0.7, 1.5]
    time.sleep(sleep_times[run.retry_count])

    if run.retry_count < 2:
        phase.retry("Retrying phase")
    else:
        
