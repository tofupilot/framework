#!/usr/bin/env python3
"""
Test script that makes all validators PASS.
This demonstrates proper validator display when all validations succeed.
"""

import sys
sys.path.insert(0, ".")

from tofupilot import Procedure

procedure = Procedure()

procedure.start_step("Multiple Lower Bounds")
procedure.measure("temperature", 25.5)

procedure.start_step("Multiple Upper Bounds")
procedure.measure("pressure", 50.0)

procedure.start_step("Range with Extra Constraint")
procedure.measure("voltage", 4.0)

procedure.start_step("Numeric Equality")
procedure.measure("exact_count", 42)

procedure.start_step("Numeric Inequality")
procedure.measure("error_code", 1)

procedure.start_step("String Equality")
procedure.measure("status", "PASS")

procedure.start_step("String Inequality")
procedure.measure("error_message", "OK")

procedure.start_step("String Pattern Matching")
procedure.measure("serial_number", "SN-123456")

procedure.start_step("String Contains")
procedure.measure("log_message", "Operation completed successfully")

procedure.start_step("Boolean Equality")
procedure.measure("system_ready", True)

procedure.start_step("Boolean Inequality")
procedure.measure("has_error", False)

procedure.start_step("Expression Validator")
procedure.measure("outlier_detection", 50.0)

procedure.start_step("Mixed Validators")
procedure.measure("sensor_reading", 50.0)

procedure.start_step("Complex Range")
procedure.measure("complex_measurement", 50.0)

procedure.finish()
print("✅ All tests PASS - All validators satisfied")
