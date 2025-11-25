"""Error handling test phases"""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))


def divide_by_zero_error(phase, run, ui):
    """Triggers a runtime error (ZeroDivisionError)"""
    print("🔥 About to trigger divide by zero error...", file=sys.stderr)
    time.sleep(0.25)
    result = 42 / 0
    
