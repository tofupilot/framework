"""Error handling test phases"""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))


def working_function(phase, test_api, ui):
    """Should pass if module has no syntax errors"""
    print("✅ Working function executed successfully", file=sys.stderr)
    time.sleep(0.25)
    


def divide_by_zero_error(phase, test_api, ui):
    """Triggers a runtime error (ZeroDivisionError)"""
    print("🔥 About to trigger divide by zero error...", file=sys.stderr)
    time.sleep(0.25)
    result = 42 / 0  # This will raise ZeroDivisionError
    


def simple_timeout(phase, test_api, ui):
    """Tests timeout handling - sleeps for 5 seconds"""
    print("⏱️ Starting timeout test - will sleep for 5 seconds...", file=sys.stderr)
    print("This phase has a 1-second timeout configured in YAML", file=sys.stderr)
    time.sleep(5)  # This will exceed the 1-second timeout
    print("This message should never appear - timeout should have occurred", file=sys.stderr)
    


def logging_demo(phase, test_api, ui):
    """Demonstrates different logging methods using test_api.logs"""
    # Note: Native print() is not captured. Use test_api.logs instead.
    test_api.log.warning("[1/4] ⚠️ Warning level log - something to watch out for")
    test_api.log.error("[2/4] ❌ Error level log - something went wrong but phase continues")
    test_api.log.debug("[3/4] 🐛 Debug level log - detailed debugging information")
    test_api.log.info("[4/4] ✅ Logs are captured and sent to the UI")

    
