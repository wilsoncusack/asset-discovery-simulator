use forge::traces::{CallKind, CallTrace, SparsedTraceArena};

// Simplified function that returns only the last relevant trace
pub fn find_last_non_proxy_call(traces: &SparsedTraceArena) -> Option<&CallTrace> {
    // Convert to a vector for easier iteration from the end
    let trace_list: Vec<&CallTrace> = traces.nodes().iter()
        .map(|node| &node.trace)
        .collect();
    
    // Use iterator methods for a more idiomatic approach
    trace_list.iter().rev()
        .find(|trace| {
            // If it's not a delegate call, it's definitely not a proxy
            if trace.kind != CallKind::DelegateCall {
                return true;
            }
            
            // For delegate calls, check if it's a pure proxy by comparing with previous trace
            let trace_idx = trace_list.iter().position(|t| t == *trace).unwrap();
            if trace_idx == 0 {
                return true; // First trace can't be a proxy of a previous one
            }
            
            // If calldata doesn't match exactly, it's not a pure proxy
            trace.data != trace_list[trace_idx - 1].data
        })
        .copied()
} 