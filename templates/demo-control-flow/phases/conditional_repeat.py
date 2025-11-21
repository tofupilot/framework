"""Phase control flow examples showing different return types."""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def conditional_repeat(phase, test_api, ui):
    """Retries until the last attempt, then passes."""
    time.sleep(0.25)
    if test_api.retry_count >= test_api.retry_limit - 1:
        
    else:
        phase.retry("Retrying phase")
