"""Error handling test phases"""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))


def simple_timeout(phase, test_api, ui):
    """Tests timeout handling - sleeps for 5 seconds"""
    print("⏱️ Starting timeout test - will sleep for 5 seconds...", file=sys.stderr)
    print("This phase has a 1-second timeout configured in YAML", file=sys.stderr)
    time.sleep(5)
    print("This message should never appear - timeout should have occurred", file=sys.stderr)
    
