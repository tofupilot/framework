import sys

def verify_plug_state(phase, run, ui):
    """Verifies plug state persisted from previous phase"""
    plugs = getattr(run, 'plugs', {})

    if 'Multimeter' in plugs:
        dmm = plugs['Multimeter']
        result = dmm.measure_voltage()
        print(f"✅ DMM measurement: {result}", file=sys.stderr)

        dmm_id = result.get('id') if isinstance(result, dict) else None
        if dmm_id:
            print(f"✅ Plug persisted - DMM ID: {dmm_id}", file=sys.stderr)
            print("🎉 Plug state persistence verified!", file=sys.stderr)
            
        else:
            print("❌ Plug lost connection - no ID found", file=sys.stderr)
            phase.fail("Phase failed")

    if 'Power Supply' in plugs:
        ps = plugs['Power Supply']
        voltage = ps.get_voltage()
        print(f"✅ Power Supply voltage: {voltage}V", file=sys.stderr)

    
