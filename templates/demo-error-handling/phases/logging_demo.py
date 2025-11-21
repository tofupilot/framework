"""Error handling test phases"""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))


def logging_demo(phase, test_api, ui):
    """Demonstrates different logging methods using test_api.logs"""
    test_api.log.warning("[1/4] ⚠️ Warning level log - something to watch out for")
    test_api.log.error("[2/4] ❌ Error level log - something went wrong but phase continues")
    test_api.log.debug("[3/4] 🐛 Debug level log - detailed debugging information")
    test_api.log.info("[4/4] ✅ Logs are captured and sent to the UI")

    
