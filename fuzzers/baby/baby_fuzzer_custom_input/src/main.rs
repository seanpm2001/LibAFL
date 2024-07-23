mod input;

#[cfg(windows)]
use std::ptr::write_volatile;
use std::{path::PathBuf, ptr::write};

use input::{
    CustomInput, CustomInputGenerator, ToggleBooleanMutator, ToggleOptionalByteArrayMutator,
};
use libafl::monitors::SimpleMonitor;
use libafl::mutators::{mapped_havoc_mutations, optional_mapped_havoc_mutations};
use libafl::{
    corpus::{InMemoryCorpus, OnDiskCorpus},
    events::SimpleEventManager,
    executors::{inprocess::InProcessExecutor, ExitKind},
    feedbacks::{CrashFeedback, MaxMapFeedback},
    fuzzer::{Fuzzer, StdFuzzer},
    mutators::scheduled::StdScheduledMutator,
    observers::StdMapObserver,
    schedulers::QueueScheduler,
    stages::mutational::StdMutationalStage,
    state::StdState,
};

use libafl_bolts::tuples::{Append, Merge};
use libafl_bolts::{current_nanos, rands::StdRand, tuples::tuple_list};

/// Coverage map with explicit assignments due to the lack of instrumentation
static mut SIGNALS: [u8; 16] = [0; 16];
static mut SIGNALS_PTR: *mut u8 = unsafe { SIGNALS.as_mut_ptr() };

/// Assign a signal to the signals map
fn signals_set(idx: usize) {
    if idx > 2 {
        println!("Setting signal: {idx}");
    }
    unsafe { write(SIGNALS_PTR.add(idx), 1) };
}

#[allow(clippy::similar_names, clippy::manual_assert)]
pub fn main() {
    // The closure that we want to fuzz
    let mut harness = |input: &CustomInput| {
        signals_set(0);
        if input.byte_array == vec![b'a'] {
            signals_set(1);
            if input.optional_byte_array == Some(vec![b'b']) {
                signals_set(2);
                if input.boolean {
                    #[cfg(unix)]
                    panic!("Artificial bug triggered =)");

                    // panic!() raises a STATUS_STACK_BUFFER_OVERRUN exception which cannot be caught by the exception handler.
                    // Here we make it raise STATUS_ACCESS_VIOLATION instead.
                    // Extending the windows exception handler is a TODO. Maybe we can refer to what winafl code does.
                    // https://github.com/googleprojectzero/winafl/blob/ea5f6b85572980bb2cf636910f622f36906940aa/winafl.c#L728
                    #[cfg(windows)]
                    unsafe {
                        write_volatile(0 as *mut u32, 0);
                    }
                }
            }
        }
        ExitKind::Ok
    };

    // Create an observation channel using the signals map
    let observer = unsafe { StdMapObserver::from_mut_ptr("signals", SIGNALS_PTR, SIGNALS.len()) };

    // Feedback to rate the interestingness of an input
    let mut feedback = MaxMapFeedback::new(&observer);

    // A feedback to choose if an input is a solution or not
    let mut objective = CrashFeedback::new();

    // create a State from scratch
    let mut state = StdState::new(
        // RNG
        StdRand::with_seed(current_nanos()),
        // Corpus that will be evolved, we keep it in memory for performance
        InMemoryCorpus::new(),
        // Corpus in which we store solutions (crashes in this example),
        // on disk so the user can get them after stopping the fuzzer
        OnDiskCorpus::new(PathBuf::from("./crashes")).unwrap(),
        // States of the feedbacks.
        // The feedbacks can report the data that should persist in the State.
        &mut feedback,
        // Same for objective feedbacks
        &mut objective,
    )
    .unwrap();

    // The Monitor trait define how the fuzzer stats are displayed to the user
    let mon = SimpleMonitor::new(|s| println!("{s}"));

    // The event manager handle the various events generated during the fuzzing loop
    // such as the notification of the addition of a new item to the corpus
    let mut mgr = SimpleEventManager::new(mon);

    // A queue policy to get testcasess from the corpus
    let scheduler = QueueScheduler::new();

    // A fuzzer with feedbacks and a corpus scheduler
    let mut fuzzer = StdFuzzer::new(scheduler, feedback, objective);

    // Create the executor for an in-process function with just one observer
    let mut executor = InProcessExecutor::new(
        &mut harness,
        tuple_list!(observer),
        &mut fuzzer,
        &mut state,
        &mut mgr,
    )
    .expect("Failed to create the Executor");

    // Generator of printable bytearrays of max size 32
    let mut generator = CustomInputGenerator::new(1);

    // Generate 8 initial inputs
    state
        .generate_initial_inputs(&mut fuzzer, &mut executor, &mut generator, &mut mgr, 8)
        .expect("Failed to generate the initial corpus");

    let mutations = mapped_havoc_mutations(
        CustomInput::byte_array_mut,
        &CustomInput::byte_array_optional,
    )
    .merge(optional_mapped_havoc_mutations(
        CustomInput::optional_byte_array_mut,
        &CustomInput::optional_byte_array_optional,
    ))
    .append(ToggleOptionalByteArrayMutator::new(1))
    .append(ToggleBooleanMutator);

    let mutator = StdScheduledMutator::new(mutations);
    let mut stages = tuple_list!(StdMutationalStage::new(mutator));

    fuzzer
        .fuzz_loop(&mut stages, &mut executor, &mut state, &mut mgr)
        .expect("Error in the fuzzing loop");
}