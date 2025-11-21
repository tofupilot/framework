def initialize_ethercat(phase, run, ethercat):
    interface = ethercat.find_interface(20, 1.0)
    if interface is None:
        phase.fail("Phase failed")

    if not ethercat.update_config(interface):
        phase.fail("Phase failed")

    slave_count = ethercat.count_slaves(10, 0.5)
    state = ethercat.get_state()

    if slave_count != state["expected_slaves"]:
        phase.fail("Phase failed")

    ethercat.cleanup_processes()

    if not ethercat.launch_broker(10):
        phase.fail("Phase failed")

    if not ethercat.launch_hal(20):
        phase.fail("Phase failed")

    
