"""Error handling test phases"""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))


def working_function(phase, test_api, ui):
    """Should pass if module has no syntax errors"""
    print("✅ Working function executed successfully", file=sys.stderr)
    time.sleep(0.25)
    
