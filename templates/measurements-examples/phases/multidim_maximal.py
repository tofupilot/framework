import sys
import os
import math
def test_maximal_multidim(phase, test_api, ui):
    """Diode I-V curve with validators, aggregations, and aggregation validators"""

    # Generate diode I-V characteristic curve
    voltages = [v * 0.01 for v in range(-200, 201)]  # -2V to +2V in 10mV steps
    currents = []

    # Diode equation: I = Is * (exp(qV/nkT) - 1)
    Is = 1e-12  # Saturation current (A)
    n = 1.2  # Ideality factor
    Vt = 0.026  # Thermal voltage at room temperature (V)

    for V in voltages:
        if V < -1.5:  # Avoid numerical overflow for large negative voltages
            I = -Is
        else:
            try:
                I = Is * (math.exp(V / (n * Vt)) - 1)
                # Add some realistic noise
                I += I * 0.01 * (0.5 - hash(str(V)) % 100 / 100)
            except OverflowError:
                I = Is * 1e10  # Large positive current for forward bias

        currents.append(I)

    # Calculate aggregations
    # Forward voltage (voltage at 1mA current)
    target_current = 0.001  # 1mA
    forward_voltage = None
    for i, current in enumerate(currents):
        if current >= target_current:
            forward_voltage = voltages[i]
            break
    if forward_voltage is None:
        forward_voltage = 0.7  # Default forward voltage

    # Reverse current (current at -1V)
    reverse_voltage_index = next((i for i, v in enumerate(voltages) if v <= -1.0), 0)
    reverse_current = abs(currents[reverse_voltage_index])

    # Dynamic resistance at forward voltage
    if forward_voltage and len(voltages) > 10:
        # Find index near forward voltage
        fv_index = min(
            range(len(voltages)), key=lambda i: abs(voltages[i] - forward_voltage)
        )
        if fv_index > 5 and fv_index < len(voltages) - 5:
            dv = voltages[fv_index + 1] - voltages[fv_index - 1]
            di = currents[fv_index + 1] - currents[fv_index - 1]
            dynamic_resistance = dv / di if di != 0 else 1000
        else:
            dynamic_resistance = 26.0  # Typical value
    else:
        dynamic_resistance = 26.0

    # Create axes with validators
    x_axis = Axis(
        data=voltages,
        validators=[
            # ⚠️ Multidimensional axis validators - outcomes only (specs should be in YAML)
            Validator(level="critical", operator=">=", outcome="PASS"),
            Validator(level="critical", operator="<=", outcome="PASS"),
        ],
    )

    current_axis = Axis(
        data=currents,
        aggregations=[
            # ⚠️ Note: Multidimensional axis aggregations - using constrained format
            Aggregation(
                type="forward_voltage",
                value=forward_voltage,
                validators=[
                    # ⚠️ Axis aggregation validator - outcome only (specs should be in YAML)
                    Validator(level="alert", operator=">", outcome="PASS")
                ],
            ),
            Aggregation(type="reverse_current", value=reverse_current),
            Aggregation(
                type="dynamic_resistance", value=dynamic_resistance
            ),
        ],
    )

    # Create multidimensional measurement
    diode_iv_curve = MultiDim(
        x_axis=x_axis,
        y_axis=[current_axis],
    )

    test_api.measurements.diode_iv_curve = diode_iv_curve

    test_api.log.info(f"Generated diode I-V curve with {len(voltages)} data points")
    test_api.log.info(f"Forward voltage (@ 1mA): {forward_voltage:.3f}V")
    test_api.log.info(f"Reverse current (@ -1V): {reverse_current:.2e}A")
    test_api.log.info(f"Dynamic resistance: {dynamic_resistance:.1f}Ω")

    
