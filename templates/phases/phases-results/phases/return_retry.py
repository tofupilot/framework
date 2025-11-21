"""Test RETRY phase return value cases."""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def return_retry():
    """Return "Retry" → Retry (stops before reaching limit)."""
    memory_size = (test_api.retry_count + 1) * 10 * 1024 * 1024
    data = bytearray(memory_size)

    sleep_times = [0.2, 0.5, 1.0]
    time.sleep(sleep_times[test_api.retry_count])

    if test_api.retry_count < 2:
        return "Retry"
    else:
        return "Pass"
