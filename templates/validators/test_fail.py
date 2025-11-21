#!/usr/bin/env python3
"""
Test script that makes some validators FAIL.
This demonstrates validator display with failing validations highlighted.
"""

import sys
sys.path.insert(0, ".")

from tofupilot import Procedure

procedure = Procedure()

procedure.start_step("Multiple Lower Bounds")
procedure.measure("temperature", 5.0)

procedure.start_step("Multiple Upper Bounds")
procedure.measure("pressure", 95.0)

procedure.start_step("Range with Extra Constraint")
procedure.measure("voltage", 4.2)

procedure.start_step("Numeric Equality")
procedure.measure("exact_count", 40)

procedure.start_step("Numeric Inequality")
procedure.measure("error_code", 0)

procedure.start_step("String Equality")
procedure.measure("status", "FAIL")

procedure.start_step("String Inequality")
procedure.measure("error_message", "FAILED")

procedure.start_step("String Pattern Matching")
procedure.measure("serial_number", "INVALID-123")

procedure.start_step("String Contains")
procedure.measure("log_message", "Operation failed with error")

procedure.start_step("Boolean Equality")
procedure.measure("system_ready", False)

procedure.start_step("Boolean Inequality")
procedure.measure("has_error", True)

procedure.start_step("Expression Validator")
procedure.measure("outlier_detection", 50.0)

procedure.start_step("Mixed Validators")
procedure.measure("sensor_reading", -5.0)

procedure.start_step("Complex Range")
procedure.measure("complex_measurement", 5.0)

procedure.finish()
print("❌ Multiple tests FAIL - Demonstrates failing validator display")
