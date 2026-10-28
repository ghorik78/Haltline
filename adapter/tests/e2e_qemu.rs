//! End-to-end tests: the real `haltline-dap` binary debugging the test
//! firmware on an emulated STM32F405 (QEMU `netduinoplus2`) through GDB.
//!
//! They are skipped when QEMU, an ARM GDB or arm-none-eabi-gcc is missing;
//! set `HALTLINE_E2E=1` to make missing tools an error (CI does this).

mod common;

use common::*;
use serde_json::json;

fn finish_config(client: &mut DapClient, breakpoints: &[(i64, serde_json::Value)]) {
    if !breakpoints.is_empty() {
        let bps: Vec<_> = breakpoints
            .iter()
            .map(|(line, extra)| {
                let mut b = json!({"line": line});
                if let (Some(o), Some(e)) = (b.as_object_mut(), extra.as_object()) {
                    o.extend(e.clone());
                }
                b
            })
            .collect();
        client.request(
            "setBreakpoints",
            json!({"source": {"path": source("main.c")}, "breakpoints": bps}),
        );
    }
    client.request("configurationDone", json!({}));
}

#[test]
fn breakpoints_stepping_and_variables() {
    let Some(tools) = tools() else { return };
    let mut c = launch(&tools, json!({}));
    let loop_line = line_of("main.c", "loop");
    finish_config(&mut c, &[]);

    // Runs to main.
    let stop = c.stopped();
    assert_eq!(stop["reason"], "entry");
    let top = c.top_frame();
    assert_eq!(top["name"], "main");
    assert_eq!(line(&top), line_of("main.c", "main_first"));
    assert!(top["source"]["path"].as_str().unwrap().ends_with("main.c"));

    // Conditional breakpoint.
    let bps = c.request(
        "setBreakpoints",
        json!({"source": {"path": source("main.c")}, "breakpoints": [{"line": loop_line, "condition": "i == 3"}]}),
    );
    let bp = &bps["breakpoints"][0];
    assert_eq!(bp["verified"], true, "{bps}");
    assert_eq!(bp["line"], loop_line);
    c.request("continue", json!({"threadId": 1}));
    let stop = c.stopped();
    assert_eq!(stop["reason"], "breakpoint");
    assert_eq!(stop["hitBreakpointIds"][0], bp["id"]);
    assert_eq!(c.evaluate("i", "watch")["result"], "3");
    assert_eq!(c.evaluate("g_counter", "hover")["result"], "3");

    // Locals, globals (with children and big arrays), statics.
    let frame_id = c.top_frame()["id"].as_i64().unwrap();
    let locals = c.scope(frame_id, "Locals");
    assert_eq!(c.variable(locals, "i")["value"], "3");
    let globals = c.scope(frame_id, "Globals");
    let point = c.variable(globals, "g_point");
    let point_ref = point["variablesReference"].as_i64().unwrap();
    assert!(point_ref > 0);
    assert_eq!(c.variable(point_ref, "x")["value"], "6");
    assert_eq!(c.variable(point_ref, "y")["value"], "1");
    let big = c.variable(globals, "g_big");
    let chunks = c.variables(big["variablesReference"].as_i64().unwrap());
    assert_eq!(chunks.len(), 3, "{chunks:?}");
    assert_eq!(chunks[0]["name"], "[0..99]");
    let first_chunk = c.variables(chunks[0]["variablesReference"].as_i64().unwrap());
    assert_eq!(first_chunk.len(), 100);
    assert_eq!(first_chunk[5]["name"], "[5]");
    assert_eq!(first_chunk[5]["evaluateName"], "g_big[5]");
    let statics = c.scope(frame_id, "Static");
    assert_eq!(c.variable(statics, "s_calls")["value"], "3");

    // Step over, into, out.
    c.request("next", json!({"threadId": 1}));
    assert_eq!(c.stopped()["reason"], "step");
    assert_eq!(line(&c.top_frame()), line_of("main.c", "call_update"));
    c.request("stepIn", json!({"threadId": 1}));
    c.stopped();
    let frames = c.frames();
    assert_eq!(frames[0]["name"], "update_point");
    assert_eq!(frames[1]["name"], "main");
    assert_eq!(line(&frames[0]), line_of("main.c", "update_x"));
    c.request("stepOut", json!({"threadId": 1}));
    c.stopped();
    assert_eq!(c.top_frame()["name"], "main");

    // Changing a variable.
    let frame_id = c.top_frame()["id"].as_i64().unwrap();
    let locals = c.scope(frame_id, "Locals");
    let set = c.request(
        "setVariable",
        json!({"variablesReference": locals, "name": "i", "value": "8"}),
    );
    assert_eq!(set["value"], "8");
    assert_eq!(c.evaluate("i", "watch")["result"], "8");

    // Log point + plain breakpoint.
    let update_line = line_of("main.c", "update_x");
    let fact_line = line_of("main.c", "call_fact");
    c.request(
        "setBreakpoints",
        json!({"source": {"path": source("main.c")}, "breakpoints": [
            {"line": update_line, "logMessage": "dx={dx} x={p->x}"},
            {"line": fact_line},
        ]}),
    );
    c.request("continue", json!({"threadId": 1}));
    let stop = c.stopped();
    assert_eq!(stop["reason"], "breakpoint");
    assert_eq!(line(&c.top_frame()), fact_line);
    assert!(c.output().contains("dx=9 x="), "{}", c.output());

    // Function breakpoint with a hit condition: factorial(5), (4), (3).
    c.request(
        "setBreakpoints",
        json!({"source": {"path": source("main.c")}, "breakpoints": []}),
    );
    let fbp = c.request(
        "setFunctionBreakpoints",
        json!({"breakpoints": [{"name": "factorial", "hitCondition": "==3"}]}),
    );
    assert_eq!(fbp["breakpoints"][0]["verified"], true);
    c.request("continue", json!({"threadId": 1}));
    let stop = c.stopped();
    assert_eq!(stop["reason"], "function breakpoint");
    assert_eq!(c.evaluate("n", "watch")["result"], "3");
    let frames = c.frames();
    assert_eq!(
        frames.iter().filter(|f| f["name"] == "factorial").count(),
        3,
        "{frames:?}"
    );
    c.disconnect();
}

#[test]
fn registers_peripherals_memory_and_disassembly() {
    let Some(tools) = tools() else { return };
    let mut c = launch(
        &tools,
        json!({"svdFile": common::firmware_dir().join("stm32f405-test.svd")}),
    );
    let loop_line = line_of("main.c", "loop");
    finish_config(&mut c, &[(loop_line, json!({}))]);
    assert_eq!(c.stopped()["reason"], "entry");
    c.request("continue", json!({"threadId": 1}));
    assert_eq!(c.stopped()["reason"], "breakpoint");

    // Registers.
    let top = c.top_frame();
    let frame_id = top["id"].as_i64().unwrap();
    let regs_ref = c.scope(frame_id, "Registers");
    let regs = c.variables(regs_ref);
    let pc = regs.iter().find(|r| r["name"] == "pc").expect("pc");
    assert_eq!(
        address(&json!(pc["value"]
            .as_str()
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap())),
        address(&top["instructionPointerReference"])
    );
    let xpsr = regs.iter().find(|r| r["name"] == "xpsr").expect("xpsr");
    let bits = c.variables(xpsr["variablesReference"].as_i64().unwrap());
    assert_eq!(
        bits.iter().find(|b| b["name"] == "T").unwrap()["value"],
        "1"
    );
    let system = regs
        .iter()
        .find(|r| r["name"] == "System")
        .expect("System group");
    let system_regs = c.variables(system["variablesReference"].as_i64().unwrap());
    assert!(system_regs.iter().any(|r| r["name"] == "msp"));
    let faults = regs
        .iter()
        .find(|r| r["name"] == "Fault status (SCB)")
        .unwrap();
    let fault_regs = c.variables(faults["variablesReference"].as_i64().unwrap());
    assert_eq!(fault_regs[0]["name"], "CFSR");
    assert_eq!(fault_regs[0]["value"], "0x00000000");

    // Changing a register.
    c.request(
        "setVariable",
        json!({"variablesReference": regs_ref, "name": "r5", "value": "0x1234"}),
    );
    assert_eq!(c.evaluate("$r5", "watch")["result"], "4660");

    // Peripherals from the SVD file.
    let periph_ref = c.scope(frame_id, "Peripherals");
    let periphs = c.variables(periph_ref);
    let names: Vec<_> = periphs
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["SCRATCH", "TIM2", "USART1", "USART2"]);
    let tim2 = c.variables(periphs[1]["variablesReference"].as_i64().unwrap());
    let arr = tim2.iter().find(|r| r["name"] == "ARR").unwrap();
    assert_eq!(arr["value"], "0x00001234");
    assert_eq!(arr["memoryReference"], "0x4000002c");
    assert_eq!(
        tim2.iter().find(|r| r["name"] == "PSC").unwrap()["value"],
        "0x00000007"
    );
    let arr_fields = c.variables(arr["variablesReference"].as_i64().unwrap());
    assert_eq!(
        arr_fields.iter().find(|f| f["name"] == "ARR_L").unwrap()["value"],
        "0x1234"
    );
    let usart = c.variables(periphs[2]["variablesReference"].as_i64().unwrap());
    let sr = usart.iter().find(|r| r["name"] == "SR").unwrap();
    assert_eq!(sr["value"], "0x000000c0");
    let sr_fields = c.variables(sr["variablesReference"].as_i64().unwrap());
    let txe = sr_fields.iter().find(|f| f["name"] == "TXE").unwrap();
    assert_eq!(txe["value"], "1");
    assert_eq!(txe["presentationHint"]["attributes"][0], "readOnly");
    let dr = usart.iter().find(|r| r["name"] == "DR").unwrap();
    assert!(dr["value"].as_str().unwrap().contains("side effects"));
    // A watchpoint can be put on a peripheral register.
    let info = c.request(
        "dataBreakpointInfo",
        json!({"variablesReference": periphs[1]["variablesReference"], "name": "ARR"}),
    );
    assert_eq!(info["dataId"], "*(unsigned int *) 0x4000002c");

    // Writing registers and fields (RAM-backed SCRATCH block, see the SVD).
    let scratch_ref = periphs[0]["variablesReference"].as_i64().unwrap();
    let set = c.request(
        "setVariable",
        json!({"variablesReference": scratch_ref, "name": "VALUE", "value": "0x55"}),
    );
    assert_eq!(set["value"], "0x00000055");
    let mem = c.request(
        "readMemory",
        json!({"memoryReference": "0x2001f004", "count": 4}),
    );
    assert_eq!(
        mem["data"],
        haltline_dap::util::base64_encode(&[0x55, 0, 0, 0])
    );
    let scratch = c.variables(scratch_ref);
    let ctrl = scratch.iter().find(|r| r["name"] == "CTRL").unwrap();
    let ctrl_ref = ctrl["variablesReference"].as_i64().unwrap();
    let set = c.request(
        "setVariable",
        json!({"variablesReference": ctrl_ref, "name": "MODE", "value": "Fast"}),
    );
    assert_eq!(set["value"], "Fast (2)");
    c.request(
        "setVariable",
        json!({"variablesReference": ctrl_ref, "name": "EN", "value": "1"}),
    );
    let out = c.evaluate("*(unsigned int *) 0x2001f000", "watch");
    assert_eq!(out["result"], "18");
    let resp = c.request_raw(
        "setVariable",
        json!({"variablesReference": scratch_ref, "name": "STATUS", "value": "1"}),
    );
    assert_eq!(resp["success"], false);
    let resp = c.request_raw(
        "setVariable",
        json!({"variablesReference": ctrl_ref, "name": "MODE", "value": "7"}),
    );
    assert_eq!(resp["success"], false);

    // Memory writes.
    let buffer = c.evaluate("&g_buffer", "repl");
    let buffer_addr = buffer["memoryReference"].as_str().unwrap().to_string();
    c.request(
        "writeMemory",
        json!({"memoryReference": buffer_addr, "offset": 1, "data": haltline_dap::util::base64_encode(&[7, 8])}),
    );
    assert_eq!(c.evaluate("g_buffer[2]", "watch")["result"], "8 '\\b'");

    // Instruction stepping shows a disassembly file.
    c.request("next", json!({"threadId": 1, "granularity": "instruction"}));
    c.stopped();
    let top = c.top_frame();
    let path = top["source"]["path"].as_str().unwrap().to_string();
    assert!(path.ends_with(".s"), "{top}");
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let current = lines[line(&top) as usize - 1];
    let pc = address(&top["instructionPointerReference"]);
    assert!(
        current.contains(&format!("0x{pc:08x}")),
        "{current} vs {pc:x}"
    );

    // A breakpoint set in the disassembly file, two instructions ahead.
    let next_lines: Vec<i64> = lines
        .iter()
        .enumerate()
        .skip(line(&top) as usize)
        .filter(|(_, l)| l.trim_start().starts_with("0x"))
        .map(|(i, _)| i as i64 + 1)
        .take(2)
        .collect();
    let target_line = next_lines[1];
    let target_addr = address(&json!(lines[target_line as usize - 1]
        .split_whitespace()
        .next()
        .unwrap()));
    let bps = c.request(
        "setBreakpoints",
        json!({"source": {"path": path}, "breakpoints": [{"line": target_line}]}),
    );
    assert_eq!(bps["breakpoints"][0]["verified"], true, "{bps}");
    c.request(
        "setBreakpoints",
        json!({"source": {"path": source("main.c")}, "breakpoints": []}),
    );
    c.request("continue", json!({"threadId": 1}));
    assert_eq!(c.stopped()["reason"], "breakpoint");
    assert_eq!(
        address(&c.top_frame()["instructionPointerReference"]),
        target_addr
    );

    // The DAP disassemble request.
    let dis = c.request(
        "disassemble",
        json!({"memoryReference": format!("0x{target_addr:x}"), "instructionOffset": -2, "instructionCount": 5}),
    );
    let insns = dis["instructions"].as_array().unwrap();
    assert_eq!(insns.len(), 5);
    assert_eq!(address(&insns[2]["address"]), target_addr);

    // Line stepping returns to the C source.
    c.request(
        "setBreakpoints",
        json!({"source": {"path": path}, "breakpoints": []}),
    );
    c.request("next", json!({"threadId": 1, "granularity": "line"}));
    c.stopped();
    assert!(c.top_frame()["source"]["path"]
        .as_str()
        .unwrap()
        .ends_with("main.c"));

    // Debug console.
    let out = c.evaluate("info registers pc", "repl");
    assert!(out["result"].as_str().unwrap().contains("pc"), "{out}");
    let out = c.evaluate(":svd TIM2.ARR", "repl");
    assert!(
        out["result"]
            .as_str()
            .unwrap()
            .contains("TIM2.ARR @ 0x4000002c = 0x00001234"),
        "{out}"
    );
    let out = c.evaluate(":mi -data-list-register-names", "repl");
    assert!(out["result"].as_str().unwrap().contains("\"r0\""));
    c.evaluate(":hex on", "repl");
    c.clear_events("stopped");
    assert!(c.evaluate("g_point.x", "watch")["result"]
        .as_str()
        .unwrap()
        .starts_with("0x"));
    let out = c.evaluate(":asm on", "repl");
    assert!(out["result"].as_str().unwrap().contains("on"));
    c.stopped();
    assert!(c.top_frame()["source"]["path"]
        .as_str()
        .unwrap()
        .ends_with(".s"));
    c.disconnect();
}

#[test]
fn faults_watchpoints_restart_and_pause() {
    let Some(tools) = tools() else { return };
    // Loading through GDB (off by default for QEMU) exercises the flashing
    // progress and the verification with the real GDB.
    let mut c = launch(
        &tools,
        json!({"loadImage": true, "verifyAfterLoad": true, "reloadOnRestart": true}),
    );
    assert!(
        c.output().contains("Flashing firmware.elf ("),
        "{}",
        c.output()
    );
    assert!(c.output().contains("Loaded and verified"), "{}", c.output());
    let filters = c.request("setExceptionBreakpoints", json!({"filters": ["faults"]}));
    assert_eq!(filters["breakpoints"][0]["verified"], true);
    finish_config(&mut c, &[]);
    assert_eq!(c.stopped()["reason"], "entry");

    // Watchpoint on a global.
    let info = c.request("dataBreakpointInfo", json!({"name": "g_counter"}));
    assert_eq!(info["dataId"], "g_counter");
    let wp = c.request(
        "setDataBreakpoints",
        json!({"breakpoints": [{"dataId": "g_counter", "accessType": "write"}]}),
    );
    assert_eq!(wp["breakpoints"][0]["verified"], true, "{wp}");
    c.request("continue", json!({"threadId": 1}));
    let stop = c.stopped();
    assert_eq!(stop["reason"], "data breakpoint", "{stop}");
    assert!(
        stop["description"].as_str().unwrap().contains("g_counter"),
        "{stop}"
    );
    c.request("setDataBreakpoints", json!({"breakpoints": []}));

    // Data breakpoint through a variable container (Globals scope).
    let frame_id = c.top_frame()["id"].as_i64().unwrap();
    let globals = c.scope(frame_id, "Globals");
    c.variables(globals);
    let info = c.request(
        "dataBreakpointInfo",
        json!({"variablesReference": globals, "name": "g_ratio"}),
    );
    assert_eq!(info["dataId"], "g_ratio");

    // HardFault through the fault filter.
    c.request(
        "setExpression",
        json!({"expression": "g_trigger_fault", "value": "1", "frameId": frame_id}),
    );
    c.request("continue", json!({"threadId": 1}));
    let stop = c.stopped();
    assert_eq!(stop["reason"], "exception", "{stop}");
    assert_eq!(stop["description"], "HardFault");
    assert!(
        stop["text"].as_str().unwrap().contains("UNDEFINSTR"),
        "{stop}"
    );
    assert_eq!(c.top_frame()["name"], "HardFault_Handler");
    assert!(c.output().contains("HardFault detected"));
    let info = c.request("exceptionInfo", json!({"threadId": 1}));
    assert_eq!(info["exceptionId"], "HardFault");
    let report = c.evaluate(":fault", "repl");
    let report = report["result"].as_str().unwrap();
    assert!(report.contains("CFSR.UNDEFINSTR"), "{report}");
    assert!(report.contains("main.c"), "{report}");
    let frames = c.frames();
    assert!(
        frames.iter().any(|f| f["name"] == "<exception entry>"),
        "{frames:?}"
    );
    assert!(
        frames.iter().any(|f| f["name"] == "fault_now"),
        "{frames:?}"
    );

    // Restart (reset) runs to main again with fresh RAM.
    c.request("restart", json!({}));
    let stop = c.stopped();
    assert_eq!(stop["reason"], "entry");
    assert_eq!(c.top_frame()["name"], "main");
    assert_eq!(c.evaluate("g_trigger_fault", "watch")["result"], "0");

    // Pause a running target.
    c.request("setExceptionBreakpoints", json!({"filters": []}));
    c.request("continue", json!({"threadId": 1}));
    std::thread::sleep(std::time::Duration::from_millis(500));
    let threads = c.request("threads", json!({}));
    assert!(!threads["threads"].as_array().unwrap().is_empty());
    c.request("pause", json!({"threadId": 1}));
    let stop = c.stopped();
    assert_eq!(stop["reason"], "pause");
    assert_eq!(line(&c.top_frame()), line_of("main.c", "idle"));

    // Breakpoints can be changed while running.
    c.request("continue", json!({"threadId": 1}));
    let bps = c.request(
        "setBreakpoints",
        json!({"source": {"path": source("main.c")}, "breakpoints": [{"line": line_of("main.c", "idle")}]}),
    );
    assert_eq!(bps["breakpoints"][0]["verified"], true);
    assert_eq!(c.stopped()["reason"], "breakpoint");

    c.request("terminate", json!({}));
    c.wait_event("terminated");
    c.disconnect();
}

#[test]
fn reports_launch_errors() {
    let Some(tools) = tools() else { return };
    let mut c = DapClient::spawn();
    c.request("initialize", json!({"adapterID": "haltline"}));
    let resp = c.request_raw(
        "launch",
        json!({"serverType": "qemu", "executable": "/no/such/file.elf"}),
    );
    assert_eq!(resp["success"], false);
    assert!(resp["message"].as_str().unwrap().contains("does not exist"));

    let elf = firmware(&tools);
    let resp = c.request_raw(
        "launch",
        json!({"serverType": "qemu", "executable": elf, "serverPath": "/no/such/qemu", "gdbPath": tools.gdb}),
    );
    assert_eq!(resp["success"], false);
    assert!(
        resp["message"].as_str().unwrap().contains("does not exist"),
        "{resp}"
    );
    c.disconnect();
}
