import sys

def case_8_measurement_fail(measurements):
    print("Recording measurement that will fail validation...", file=sys.stderr)
    measurements.voltage = 2.5
    print("Measurement recorded: 2.5V (expected >= 3.0V) → FAIL outcome", file=sys.stderr)
