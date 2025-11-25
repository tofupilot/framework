"""Error handling test phases"""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))


def working_function(phase, run, ui):
    """Should pass if module has no syntax errors"""
    print("✅ Working function executed successfully", file=sys.stderr)
    time.sleep(0.25)
    


def divide_by_zero_error(phase, run, ui):
    """Triggers a runtime error (ZeroDivisionError)"""
    print("🔥 About to trigger divide by zero error...", file=sys.stderr)
    time.sleep(0.25)
    result = 42 / 0  # This will raise ZeroDivisionError
    


def simple_timeout(phase, run, ui):
    """Tests timeout handling - sleeps for 5 seconds"""
    print("⏱️ Starting timeout test - will sleep for 5 seconds...", file=sys.stderr)
    print("This phase has a 1-second timeout configured in YAML", file=sys.stderr)
    time.sleep(5)  # This will exceed the 1-second timeout
    print("This message should never appear - timeout should have occurred", file=sys.stderr)
    


def logging_demo(phase, run, ui):
    """Demonstrates different logging methods using run.logs"""
    # Note: Native print() is not captured. Use run.logs instead.
    run.log.warning("[1/4] ⚠️ Warning level log - something to watch out for")
    run.log.error("[2/4] ❌ Error level log - something went wrong but phase continues")
    run.log.debug("[3/4] 🐛 Debug level log - detailed debugging information")
    run.log.info("[4/4] ✅ Logs are captured and sent to the UI")

    
