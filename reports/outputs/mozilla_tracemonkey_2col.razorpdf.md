# Trace-based Just-in-Time Type Specialization for Dynamic

# Languages

### Andreas Gal∗+ , Brendan Eich, Mike Shaver, David Anderson, David Mandelin,

∗

∗

∗

∗

### Mohammad R. Haghighat$, Blake Kaplan, Graydon Hoare, Boris Zbarsky, Jason Orendorff,

∗

∗

∗

∗

### Jesse Ruderman∗, Edwin Smith, Rick Reitmaier, Michael Bebenita, Mason Chang, Michael Franz

#

#

+

+#

+

Mozilla Corporation∗

{gal,brendan,shaver,danderson,dmandelin,mrbkap,graydon,bz,jorendorff,jruderman}@mozilla.com

Adobe Corporation#

{edwsmith,rreitmai}@adobe.com

Intel Corporation$

{mohammad.r.haghighat}@intel.com

University of California, Irvine+

{mbebenit,changm,franz}@uci.edu

### Abstract

and is used for the application logic of browser-based productivity

Dynamic languages such as JavaScript are more difficult to com-applications such as Google Mail, Google Docs and Zimbra Col-

pile than statically typed ones. Since no concrete type informationlaboration Suite. In this domain, in order to provide a fluid user

is available, traditional compilers need to emit generic code that canexperience and enable a new generation of applications, virtual ma-

handle all possible type combinations at runtime. We present an al-chines must provide a low startup time and high performance.

ternative compilation technique for dynamically-typed languagesCompilers for statically typed languages rely on type informa-

that identifies frequently executed loop traces at run-time and thention to generate efficient machine code. In a dynamically typed pro-

generates machine code on the fly that is specialized for the ac-gramming language such as JavaScript, the types of expressions

tual dynamic types occurring on each path through the loop. Ourmay vary at runtime. This means that the compiler can no longer

method provides cheap inter-procedural type specialization, and aneasily transform operations into machine instructions that operate

elegant and efficient way of incrementally compiling lazily discov-on one specific type. Without exact type information, the compiler

ered alternative paths through nested loops. We have implementedmust emit slower generalized machine code that can deal with all

a dynamic compiler for JavaScript based on our technique and wepotential type combinations. While compile-time static type infer-

have measured speedups of 10x and more for certain benchmarkence might be able to gather type information to generate opti-

mized machine code, traditional static analysis is very expensive

programs.

and hence not well suited for the highly interactive environment of

Categories and Subject DescriptorsD.3.4 [Programming Lan- a web browser.

guages]: Processors —Incremental compilers, code generation. We present a trace-based compilation technique for dynamic

languages that reconciles speed of compilation with excellent per-

General TermsDesign, Experimentation, Measurement, Perfor-

formance of the generated machine code. Our system uses a mixed-

mance.

mode execution approach: the system starts running JavaScript in a

fast-starting bytecode interpreter. As the program runs, the system

Keywords JavaScript, just-in-time compilation, trace trees.

identifieshot(frequently executed) bytecode sequences, records

them, and compiles them to fast native code. We call such a se-

### 1. Introduction

quence of instructions atrace.

Dynamic languagessuch as JavaScript, Python, and Ruby, are pop-Unlike method-based dynamic compilers, our dynamic com-

ular since they are expressive, accessible to non-experts, and makepiler operates at the granularity of individual loops. This design

deployment as easy as distributing a source file. They are used forchoice is based on the expectation that programs spend most of

small scripts as well as for complex applications. JavaScript, fortheir time in hot loops. Even in dynamically typed languages, we

example, is the de facto standard for client-side web programmingexpect hot loops to be mostlytype-stable, meaning that the types of

values are invariant. (12) For example, we would expect loop coun-

ters that start as integers to remain integers for all iterations. When

both of these expectations hold, a trace-based compiler can cover

Permission to make digital or hard copies of all or part of this work for personal orthe program execution with a small number of type-specialized, ef-

classroom use is granted without fee provided that copies are not made or distributedficiently compiled traces.

for profit or commercial advantage and that copies bear this notice and the full citationEach compiled trace covers one path through the program with

on the first page. To copy otherwise, to republish, to post on servers or to redistribute

one mapping of values to types. When the VM executes a compiled

to lists, requires prior specific permission and/or a fee.

trace, it cannot guarantee that the same path will be followed

PLDI’09, June 15–20, 2009, Dublin, Ireland.

or that the same types will occur in subsequent loop iterations.

Copyrightc 2009 ACM 978-1-60558-392-1/09/06...$5.00




1 for (var i = 2; i < 100; ++i) {

Hence, recording and compiling a tracespeculatesthat the path and

typing will be exactly as they were during recording for subsequent2 if (!primes[i])

3 continue;

iterations of the loop.

4 for (var k = i + i; i < 100; k += i)

Every compiled trace contains all theguards(checks) required

to validate the speculation. If one of the guards fails (if control5 primes[k] = false;

flow is different, or a value of a different type is generated), the6 }

trace exits. If an exit becomes hot, the VM can record abranch

tracestarting at the exit to cover the new path. In this way, the VMFigure 1. Sample program: sieve of Eratosthenes.primesis

records atrace treecovering all the hot paths through the loop.initialized to an array of 100falsevalues on entry to this code

Nested loops can be difficult to optimize for tracing VMs. Insnippet.

a na ̈ıve implementation, inner loops would become hot first, and

the VM would start tracing there. When the inner loop exits, the

VM would detect that a different branch was taken. The VM would

try to record a branch trace, and find that the trace reaches not the

### Symbol Key

inner loop header, but the outer loop header. At this point, the VM

### Interpret



Overhead  Interpreting Native

Bytecodes

could continue tracing until it reaches the inner loop header again,

thus tracing the outer loop inside a trace tree for the inner loop.

loop

cold/blacklisted

edge

But this requires tracing a copy of the outer loop for every side exit

loop/exit

and type combination in the inner loop. In essence, this is a formabort

compiled trace

Monitor

of unintended tail duplication, which can easily overflow the coderecording

ready

cache. Alternatively, the VM could simply stop tracing, and give upRecord hot

Enter  Compiled T

loop/exit

LIR Trace

on ever tracing outer loops.

finish at

We solve the nested loop problem by recordingnested trace

loop edge with

loop header

̈

trees. Our system traces the inner loop exactly as the naıve version.

same types

The system stops extending the inner tree when it reaches an outerCompile

Execute

loop, but then it starts a new trace at the outer loop header. WhenLIR Trace

Compiled T

the outer loop reaches the inner loop header, the system tries to call

side exit,

the trace tree for the inner loop. If the call succeeds, the VM records

side exit to

no existing trace

existing trace

the call to the inner tree as part of the outer trace and finishes

Leave

Compiled T

the outer trace as normal. In this way, our system can trace any

number of loops nested to any depth without causing excessive tail

duplication.

These techniques allow a VM to dynamically translate a pro-Figure 2.State machine describing the major activities of Trace-

gram to nested, type-specialized trace trees. Because traces canMonkey and the conditions that cause transitions to a new activ-

cross function call boundaries, our techniques also achieve the ef-ity. In the dark box, TM executes JS as compiled traces. In the

fects of inlining. Because traces have no internal control-flow joins,light gray boxes, TM executes JS in the standard interpreter. White

they can be optimized in linear time by a simple compiler (10).boxes are overhead. Thus, to maximize performance, we need to

Thus, our tracing VM efficiently performs the same kind of op-maximize time spent in the darkest box and minimize time spent in

timizations that would require interprocedural analysis in a staticthe white boxes. The best case is a loop where the types at the loop

optimization setting. This makes tracing an attractive and effectiveedge are the same as the types on entry–then TM can stay in native

tool to type specialize even complex function call-rich code.code until the loop is done.

We implemented these techniques for an existing JavaScript in-

terpreter, SpiderMonkey. We call the resulting tracing VMTrace-

a set of industry benchmarks. The paper ends with conclusions in

Monkey. TraceMonkey supports all the JavaScript features of Spi-

derMonkey, with a 2x-20x speedup for traceable programs.Section 9 and an outlook on future work is presented in Section 10.

This paper makes the following contributions:

### 2. Overview: Example Tracing Run

• We explain an algorithm for dynamically forming trace trees to

cover a program, representing nested loops as nested trace trees.This section provides an overview of our system by describing

how TraceMonkey executes an example program. The example

• We explain how to speculatively generate efficient type-specializedprogram, shown in Figure 1, computes the first 100 prime numbers

code for traces from dynamic language programs.

with nested loops. The narrative should be read along with Figure 2,

• We validate our tracing techniques in an implementation basedwhich describes the activities TraceMonkey performs and when it

on the SpiderMonkey JavaScript interpreter, achieving 2x-20xtransitions between the loops.

TraceMonkey always begins executing a program in the byte-

speedups on many programs.

code interpreter. Every loop back edge is a potential trace point.

The remainder of this paper is organized as follows. Section 3 isWhen the interpreter crosses a loop edge, TraceMonkey invokes

a general overview of trace tree based compilation we use to cap-thetrace monitor, which may decide to record or execute a native

ture and compile frequently executed code regions. In Section 4trace. At the start of execution, there are no compiled traces yet, so

we describe our approach of covering nested loops using a num-the trace monitor counts the number of times each loop back edge is

ber of individual trace trees. In Section 5 we describe our trace-executed until a loop becomeshot, currently after 2 crossings. Note

compilation based speculative type specialization approach we usethat the way our loops are compiled, the loop edge is crossed before

to generate efficient machine code from recorded bytecode traces.entering the loop, so the second crossing occurs immediately after

Our implementation of a dynamic type-specializing compiler forthe first iteration.

JavaScript is described in Section 6. Related work is discussed inHere is the sequence of events broken down by outer loop

Section 8. In Section 7 we evaluate our dynamic compiler based oniteration:


v0 := ld state[748] // load primes from the trace activation record

st sp[0], v0 // store primes to interpreter stack

v1 := ld state[764] // load k from the trace activation record

v2 := i2f(v1) // convert k from int to double

st sp[8], v1 // store k to interpreter stack

st sp[16], 0 // store false to interpreter stack

v3 := ld v0[4] // load class word for primes

v4 := and v3, -4 // mask out object class tag for primes

v5 := eq v4, Array // test whether primes is an array

xf v5 // side exit if v5 is false

v6 := js_Array_set(v0, v2, false) // call function to set array element

v7 := eq v6, 0 // test return value from call

xt v7 // side exit if js_Array_set returns false.

Figure 3. LIR snippet for sample program.This is the LIR recorded for line 5 of the sample program in Figure 1. The LIR encodes

the semantics in SSA form using temporary variables. The LIR also encodes all the stores that the interpreter would do to its data stack.

Sometimes these stores can be optimized away as the stack locations are live only on exits to the interpreter. Finally, the LIR records guards

and side exits to verify the assumptions made in this recording: thatprimesis an array and that the call to set its element succeeds.

mov edx, ebx(748) // load primes from the trace activation record

mov edi(0), edx // (*) store primes to interpreter stack

mov esi, ebx(764) // load k from the trace activation record

mov edi(8), esi // (*) store k to interpreter stack

mov edi(16), 0 // (*) store false to interpreter stack

mov eax, edx(4) // (*) load object class word for primes

and eax, -4 // (*) mask out object class tag for primes

cmp eax, Array // (*) test whether primes is an array

jne side_exit_1 // (*) side exit if primes is not an array

sub esp, 8 // bump stack for call alignment convention

push false // push last argument for call

push esi // push first argument for call

call js_Array_set // call function to set array element

add esp, 8 // clean up extra stack space

mov ecx, ebx // (*) created by register allocator

test eax, eax // (*) test return value of js_Array_set

je side_exit_2 // (*) side exit if call failed

...

side_exit_1:

mov ecx, ebp(-4) // restore ecx

mov esp, ebp // restore esp

jmp epilog // jump to ret statement

Figure 4. x86 snippet for sample program.This is the x86 code compiled from the LIR snippet in Figure 3. Most LIR instructions compile

to a single x86 instruction. Instructions marked with(*)would be omitted by an idealized compiler that knew that none of the side exits

would ever be taken. The 17 instructions generated by the compiler compare favorably with the 100+ instructions that the interpreter would

execute for the same code snippet, including 4 indirect jumps.

i=2.This is the first iteration of the outer loop. The loop oninterpreter PC and the types of values match those observed when

lines 4-5 becomes hot on its second iteration, so TraceMonkey en-trace recording was started. The first trace in our example,T45,

ters recording mode on line 4. In recording mode, TraceMonkeycovers lines 4 and 5. This trace can be entered if the PC is at line 4,

records the code along the trace in a low-level compiler intermedi-iandkare integers, andprimesis an object. After compilingT45,

ate representation we callLIR. The LIR trace encodes all the oper-TraceMonkey returns to the interpreter and loops back to line 1.

ations performed and the types of all operands. The LIR trace alsoi=3.Now the loop header at line 1 has become hot, so Trace-

encodesguards, which are checks that verify that the control flowMonkey starts recording. When recording reaches line 4, Trace-

and types are identical to those observed during trace recording.Monkey observes that it has reached an inner loop header that al-

Thus, on later executions, if and only if all guards are passed, theready has a compiled trace, so TraceMonkey attempts to nest the

trace has the required program semantics.

inner loop inside the current trace. The first step is to call the inner

TraceMonkey stops recording when execution returns to thetrace as a subroutine. This executes the loop on line 4 to completion

loop header or exits the loop. In this case, execution returns to theand then returns to the recorder. TraceMonkey verifies that the call

loop header on line 4.

was successful and then records the call to the inner trace as part of

After recording is finished, TraceMonkey compiles the trace tothe current trace. Recording continues until execution reaches line

native code using the recorded type information for optimization.1, and at which point TraceMonkey finishes and compiles a trace

The result is a native code fragment that can be entered if thefor the outer loop,T16.


A trace records all its intermediate values in a small activation

i=4.On this iteration, TraceMonkey callsT16. Becausei=4, the

record area. To make variable accesses fast on trace, the trace also

ifstatement on line 2 is taken. This branch was not taken in the

imports local and global variables by unboxing them and copying

original trace, so this causesT16 to fail a guard and take a side exit.

The exit is not yet hot, so TraceMonkey returns to the interpreter,them to its activation record. Thus, the trace can read and write

these variables with simple loads and stores from a native activation

which executes the continue statement.

recording, independently of the boxing mechanism used by the

i=5.TraceMonkey callsT16, which in turn calls the nested trace

interpreter. When the trace exits, the VM boxes the values from

T45.T16 loops back to its own header, starting the next iteration

this native storage location and copies them back to the interpreter

without ever returning to the monitor.

structures.

i=6.On this iteration, the side exit on line 2 is taken again. This

For every control-flow branch in the source program, the

time, the side exit becomes hot, so a traceT23,1 is recorded that

covers line 3 and returns to the loop header. Thus, the end ofT23,1 recorder generates conditional exit LIR instructions. These instruc-

tions exit from the trace if required control flow is different from

jumps directly to the start ofT16. The side exit is patched so that

what it was at trace recording, ensuring that the trace instructions

on future iterations, it jumps directly toT23,1.

At this point, TraceMonkey has compiled enough traces to coverare run only if they are supposed to. We call these instructions

the entire nested loop structure, so the rest of the program runsguardinstructions.

Most of our traces represent loops and end with the specialloop

entirely as native code.

LIR instruction. This is just an unconditional branch to the top of

the trace. Such traces return only via guards.

### 3. Trace Trees

Now, we describe the key optimizations that are performed as

In this section, we describe traces, trace trees, and how they arepart of recording LIR. All of these optimizations reduce complex

formed at run time. Although our techniques apply to any dynamicdynamic language constructs to simple typed constructs by spe-

language interpreter, we will describe them assuming a bytecodecializing for the current trace. Each optimization requires guard in-

structions to verify their assumptions about the state and exit the

interpreter to keep the exposition simple.

trace if necessary.

Type specialization.

3.1 Traces

All LIR primitives apply to operands of specific types. Thus,

A traceis simply a program path, which may cross function callLIR traces are necessarily type-specialized, and a compiler can

boundaries. TraceMonkey focuses onloop traces, that originate ateasily produce a translation that requires no type dispatches. A

a loop edge and represent a single iteration through the associatedtypical bytecode interpreter carries tag bits along with each value,

loop.

and to perform any operation, must check the tag bits, dynamically

Similar to an extended basic block, a trace is only entered atdispatch, mask out the tag bits to recover the untagged value,

the top, but may have many exits. In contrast to an extended basicperform the operation, and then reapply tags. LIR omits everything

block, a trace can contain join nodes. Since a trace always onlyexcept the operation itself.

follows one single path through the original program, however, joinA potential problem is that some operations can produce values

nodes are not recognizable as such in a trace and have a singleof unpredictable types. For example, reading a property from an

predecessor node like regular nodes.

object could yield a value of any type, not necessarily the type

A typed traceis a trace annotated with a type for every variableobserved during recording. The recorder emits guard instructions

(including temporaries) on the trace. A typed trace also has an entrythat conditionally exit if the operation yields a value of a different

type mapgiving the required types for variables used on the tracetype from that seen during recording. These guard instructions

before they are defined. For example, a trace could have a type mapguarantee that as long as execution is on trace, the types of values

(x: int, b: boolean), meaning that the trace may be enteredmatch those of the typed trace. When the VM observes a side exit

only if the value of the variablexis of typeintand the value ofb along such a type guard, a new typed trace is recorded originating

is of typeboolean. The entry type map is much like the signatureat the side exit location, capturing the new type of the operation in

of a function.

question.

In this paper, we only discuss typed loop traces, and we willRepresentation specialization: objects.In JavaScript, name

refer to them simply as “traces”. The key property of typed looplookup semantics are complex and potentially expensive because

traces is that they can be compiled to efficient machine code usingthey include features like object inheritance andeval. To evaluate

the same techniques used for typed languages.

an object property read expression likeo.x, the interpreter must

In TraceMonkey, traces are recorded in trace-flavored SSALIR search the property map ofoand all of its prototypes and parents.

(low-level intermediate representation). In trace-flavored SSA (orProperty maps can be implemented with different data structures

TSSA), phi nodes appear only at the entry point, which is reached(e.g., per-object hash tables or shared hash tables), so the search

both on entry and via loop edges. The important LIR primitivesprocess also must dispatch on the representation of each object

are constant values, memory loads and stores (by address andfound during search. TraceMonkey can simply observe the result of

offset), integer operators, floating-point operators, function calls,the search process and record the simplest possible LIR to access

and conditional exits. Type conversions, such as integer to double,the property value. For example, the search might finds the value of

are represented by function calls. This makes the LIR used byo.xin the prototype ofo, which uses a shared hash-table represen-

TraceMonkey independent of the concrete type system and typetation that placesxin slot 2 of a property vector. Then the recorded

conversion rules of the source language. The LIR operations arecan generate LIR that readso.xwith just two or three loads: one to

generic enough that the backend compiler is language independent.get the prototype, possibly one to get the property value vector, and

Figure 3 shows an example LIR trace.

one more to get slot 2 from the vector. This is a vast simplification

Bytecode interpreters typically represent values in a variousand speedup compared to the original interpreter code. Inheritance

complex data structures (e.g., hash tables) in a boxed format (i.e.,relationships and object representations can change during execu-

with attached type tag bits). Since a trace is intended to representtion, so the simplified code requires guard instructions that ensure

efficient code that eliminates all that complexity, our traces oper-the object representation is the same. In TraceMonkey, objects’ rep-

ate on unboxed values in simple variables and arrays as much as

possible.


resentations are assigned an integer key called theobject shape.

Starting a tree.Tree trees always start at loop headers, because

Thus, the guard is a simple equality check on the object shape.they are a natural place to look for hot paths. In TraceMonkey, loop

Representation specialization: numbers.JavaScript has no headers are easy to detect–the bytecode compiler ensures that a

integer type, only a Number type that is the set of 64-bit IEEE-bytecode is a loop header iff it is the target of a backward branch.

754 floating-pointer numbers (“doubles”). But many JavaScriptTraceMonkey starts a tree when a given loop header has been exe-

operators, in particular array accesses and bitwise operators, reallycuted a certain number of times (2 in the current implementation).

operate on integers, so they first convert the number to an integer,Starting a tree just means starting recording a trace for the current

1

and then convert any integer result back to a double.Clearly, a point and type map and marking the trace as the root of a tree. Each

JavaScript VM that wants to be fast must find a way to operate ontree is associated with a loop header and type map, so there may be

several trees for a given loop header.

integers directly and avoid these conversions.

In TraceMonkey, we support two representations for numbers:Closing the loop.Trace recording can end in several ways.

integers and doubles. The interpreter uses integer representationsIdeally, the trace reaches the loop header where it started with

as much as it can, switching for results that can only be representedthe same type map as on entry. This is called atype-stableloop

as doubles. When a trace is started, some values may be importediteration. In this case, the end of the trace can jump right to the

and represented as integers. Some operations on integers requirebeginning, as all the value representations are exactly as needed to

guards. For example, adding two integers can produce a value tooenter the trace. The jump can even skip the usual code that would

copy out the state at the end of the trace and copy it back in to the

large for the integer representation.

trace activation record to enter a trace.

Function inlining.LIR traces can cross function boundaries

in either direction, achieving function inlining. Move instructionsIn certain cases the trace might reach the loop header with a

need to be recorded for function entry and exit to copy argumentsdifferent type map. This scenario is sometime observed for the first

in and return values out. These move statements are then optimizediteration of a loop. Some variables inside the loop might initially be

away by the compiler using copy propagation. In order to be ableundefined, before they are set to a concrete type during the first loop

to return to the interpreter, the trace must also generate LIR toiteration. When recording such an iteration, the recorder cannot

record that a call frame has been entered and exited. The framelink the trace back to its own loop header since it istype-unstable.

entry and exit LIR saves just enough information to allow theInstead, the iteration is terminated with a side exit that will always

intepreter call stack to be restored later and is much simpler thanfail and return to the interpreter. At the same time a new trace is

the interpreter’s standard call code. If the function being enteredrecorded with the new type map. Every time an additional type-

is not constant (which in JavaScript includes any call by functionunstable trace is added to a region, its exit type map is compared to

name), the recorder must also emit LIR to guard that the functionthe entry map of all existing traces in case they complement each

other. With this approach we are able to cover type-unstable loop

is the same.

iterations as long they eventually form a stable equilibrium.

Guards and side exits.Each optimization described above

requires one or more guards to verify the assumptions made inFinally, the trace might exit the loop before reaching the loop

doing the optimization. A guard is just a group of LIR instructionsheader, for example because execution reaches abreakorreturn

that performs a test and conditional exit. The exit branches to astatement. In this case, the VM simply ends the trace with an exit

to the trace monitor.

side exit, a small off-trace piece of LIR that returns a pointer to

a structure that describes the reason for the exit along with theAs mentioned previously, we may speculatively chose to rep-

interpreter PC at the exit point and any other data needed to restoreresent certain Number-typed values as integers on trace. We do so

when we observe that Number-typed variables contain an integer

the interpreter’s state structures.

value at trace entry. If during trace recording the variable is unex-

Aborts.Some constructs are difficult to record in LIR traces.

pectedly assigned a non-integer value, we have to widen the type

For example,evalor calls to external functions can change the

program state in unpredictable ways, making it difficult for theof the variable to a double. As a result, the recorded trace becomes

tracer to know the current type map in order to continue tracing.inherently type-unstable since it starts with an integer value but

A tracing implementation can also have any number of other limi-ends with a double value. This represents a mis-speculation, since

tations, e.g.,a small-memory device may limit the length of traces.at trace entry we specialized the Number-typed value to an integer,

When any situation occurs that prevents the implementation fromassuming that at the loop edge we would again find an integer value

in the variable, allowing us to close the loop. To avoid future spec-

continuing trace recording, the implementationabortstrace record-

ulative failures involving this variable, and to obtain a type-stable

ing and returns to the trace monitor.

trace we note the fact that the variable in question as been observed

to sometimes hold non-integer values in an advisory data structure

3.2 Trace Trees

which we call the “oracle”.

Especially simple loops, namely those where control flow, valueWhen compiling loops, we consult the oracle before specializ-

types, value representations, and inlined functions are all invariant,ing values to integers. Speculation towards integers is performed

can be represented by a single trace. But most loops have at leastonly if no adverse information is known to the oracle about that

some variation, and so the program will take side exits from theparticular variable. Whenever we accidentally compile a loop that

main trace. When a side exit becomes hot, TraceMonkey starts ais type-unstable due to mis-speculation of a Number-typed vari-

newbranch tracefrom that point and patches the side exit to jumpable, we immediately trigger the recording of a new trace, which

directly to that trace. In this way, a single trace expands on demandbased on the now updated oracle information will start with a dou-

to a single-entry, multiple-exittrace tree.

ble value and thus become type stable.

This section explains how trace trees are formed during execu-Extending a tree.Side exits lead to different paths through

tion. The goal is to form trace trees during execution that cover allthe loop, or paths with different types or representations. Thus, to

the hot paths of the program.

completely cover the loop, the VM must record traces starting at all

side exits. These traces are recorded much like root traces: there is

a counter for each side exit, and when the counter reaches a hotness

1Arrays are actually worse than this: if the index value is a number, it mustthreshold, recording starts. Recording stops exactly as for the root

be converted from a double to a string for the property access operator, andtrace, using the loop header of the root trace as the target to reach.

then to an integer internally to the array implementation.


Our implementation does not extend at all side exits. It extends

only if the side exit is for a control-flow branch, and only if the sideT

exit does not leave the loop. In particular we do not want to extend

### Tree Anchor

a trace tree along a path that leads to an outer loop, because we

### Trunk Trace

<!-- empty unresolved VLM region -->

want to cover such paths in an outer tree through treenesting.

### Trace Anchor

### Branch Trace

3.3 Blacklisting

### Guard

Sometimes, a program follows a path that cannot be compiled

### Side Exit

into a trace, usually because of limitations in the implementation.

TraceMonkey does not currently support recording throwing and

catching of arbitrary exceptions. This design trade off was chosen,

because exceptions are usually rare in JavaScript. However, if a

program opts to use exceptions intensively, we would suddenly

incur a punishing runtime overhead if we repeatedly try to record

a trace for this path and repeatedly fail to do so, since we abort

tracing every time we observe an exception being thrown.Figure 5.A tree with two traces, a trunk trace and one branch

As a result, if a hot loop contains traces that always fail, the VMtrace. The trunk trace contains a guard to which a branch trace was

could potentially run much more slowly than the base interpreter:attached. The branch trace contain a guard that may fail and trigger

the VM repeatedly spends time trying to record traces, but is nevera side exit. Both the trunk and the branch trace loop back to the tree

able to run any. To avoid this problem, whenever the VM is aboutanchor, which is the beginning of the trace tree.

to start tracing, it must try to predict whether it will finish the trace.

Our prediction algorithm is based on blacklisting traces that have been tried and failed. When the VM fails to finish a trace start- T r ace 1 T r ace 2 ing at a given point, the VM records that a failure has occurred. The Number VM also sets a counter so that it will not try to record a trace starting at that point until it is passed a few more times (32 in our imple- mentation). This backoff counter gives temporary conditions that prevent tracing a chance to end. For example, a loop may behave Number Number differently during startup than during its steady-state execution. Af- Link ed Closed ter a given number of failures (2 in our implementation), the VM (a) marks the fragment as blacklisted, which means the VM will never again start recording at that point. T r ace 1 After implementing this basic strategy, we observed that for small loops that get blacklisted, the system can spend a noticeable Number

T r ace 1 Link ed

### Trace 2

### Boolean

### Number

### Boolean

### Boolean

### Number

Link ed T r ace 3 String String

### (b)

### Trace 2

### Boolean

amount of time just finding the loop fragment and determining that

it has been blacklisted. We now avoid that problem by patching the

bytecode. We define an extra no-op bytecode that indicates a loop

### Number

### String

header. The VM calls into the trace monitor every time the inter-String

preter executes a loop header no-op. To blacklist a fragment, we

### Linked

### Linked

### Closed

### Linked

simply replace the loop header no-op with a regular no-op. Thus,

### (c)

the interpreter will never again even call into the trace monitor.

There is a related problem we have not yet solved, which occurs

Figure 6.We handle type-unstable loops by allowing traces to

when a loop meets all of these conditions:

compile that cannot loop back to themselves due to a type mis-

• The VM can form at least one root trace for the loop.match. As such traces accumulate, we attempt to connect their loop

• There is at least one hot side exit for which the VM cannotedges to form groups of trace trees that can execute without having

to side-exit to the interpreter to cover odd type cases. This is par-

complete a trace.

ticularly important for nested trace trees where an outer tree tries to

• The loop body is short.

call an inner tree (or in this case a forest of inner trees), since inner

In this case, the VM will repeatedly pass the loop header, searchloops frequently have initially undefined values which change type

for a trace, find it, execute it, and fall back to the interpreter.to a concrete value after the first iteration.

With a short loop body, the overhead of finding and calling the

trace is high, and causes performance to be even slower than the

basic interpreter. So far, in this situation we have improved thethrough the inner loop,{i2, i3, i5, α}. Theα symbol is used to

implementation so that the VM can complete the branch trace.indicate that the trace loops back the tree anchor.

But it is hard to guarantee that this situation will never happen.When execution leaves the inner loop, the basic design has two

As future work, this situation could be avoided by detecting andchoices. First, the system can stop tracing and give up on compiling

blacklisting loops for which the average trace call executes fewthe outer loop, clearly an undesirable solution. The other choice is

to continue tracing, compiling traces for the outer loop inside the

bytecodes before returning to the interpreter.

inner loop’s trace tree.

For example, the program might exit ati5 and record a branch

### 4. Nested Trace Tree Formation

trace that incorporates the outer loop:{i5, i7, i1, i6, i7, i1, α}.

Figure 7 shows basic trace tree compilation (11) applied to a nestedLater, the program might take the other branch ati2 and then

loop where the inner loop contains two paths. Usually, the innerexit, recording another branch trace incorporating the outer loop:

loop (with header ati2) becomes hot first, and a trace tree is rooted{i2, i4, i5, i7, i1, i6, i7, i1, α}. Thus, the outer loop is recorded and

at that point. For example, the first recorded trace may be a cyclecompiled twice, and both copies must be retained in the trace cache.


### t1

### Outer Tree

### i1

### i1

### t1

### Nested Tree

### Tree Call

### t2

### i2

### i2

### Nested Tree

### i3

### t2

### i6

### i3

### i4

### Exit Guard

### i4

### t4

### i5

### i5

### Exit Guard

### i7

### i6

### (b)

### (a)

Figure 8.Control flow graph of a loop with two nested loops (left)

and its nested trace tree configuration (right). The outer tree calls

Figure 7.Control flow graph of a nested loop with an if statementthe two inner nested trace trees and places guards at their side exit

inside the inner most loop (a). An inner tree captures the innerlocations.

loop, and is nested inside an outer tree which “calls” the inner tree.

The inner tree returns to the outer tree once it exits along its loop

condition guard (b).

loop is entered withm different type maps (on geometric average),

then we compileO(m k)copies of the innermost loop. As long as

m is close to 1, the resulting trace trees will be tractable.

In general, if loops are nested to depthk, and each loop hasnpaths

An important detail is that the call to the inner trace tree must act

k

(on geometric average), this na ̈ıve strategy yieldsO(n )traces,

like a function call site: it must return to the same point every time.

which can easily fill the trace cache.

The goal of nesting is to make inner and outer loops independent;

In order to execute programs with nested loops efficiently, a

thus when the inner tree is called, it must exit to the same point

tracing system needs a technique for covering the nested loops with

in the outer tree every time with the same type map. Because we

native code without exponential trace duplication.

cannot actually guarantee this property, we must guard on it after

the call, and side exit if the property does not hold. A common

4.1 Nesting Algorithm

reason for the inner tree not to return to the same point would

The key insight is that if each loop is represented by its own tracebe if the inner tree took a new side exit for which it had never

tree, the code for each loop can be contained only in its own tree,compiled a trace. At this point, the interpreter PC is in the inner

and outer loop paths will not be duplicated. Another key fact is thattree, so we cannot continue recording or executing the outer tree.

we are not tracing arbitrary bytecodes that might have irreduceableIf this happens during recording, we abort the outer trace, to give

control flow graphs, but rather bytecodes produced by a compilerthe inner tree a chance to finish growing. A future execution of the

for a language with structured control flow. Thus, given two loopouter tree would then be able to properly finish and record a call to

edges, the system can easily determine whether they are nestedthe inner tree. If an inner tree side exit happens during execution of

and which is the inner loop. Using this knowledge, the system cana compiled trace for the outer tree, we simply exit the outer trace

compile inner and outer loops separately, and make the outer loop’sand start recording a new branch in the inner tree.

tracescallthe inner loop’s trace tree.

The algorithm for building nested trace trees is as follows. We

4.2 Blacklisting with Nesting

start tracing at loop headers exactly as in the basic tracing system.

When we exit a loop (detected by comparing the interpreter PCThe blacklisting algorithm needs modification to work well with

with the range given by the loop edge), we stop the trace. Thenesting. The problem is that outer loop traces often abort during

key step of the algorithm occurs when we are recording a tracestartup (because the inner tree is not available or takes a side exit),

which would lead to their being quickly blacklisted by the basic

for loopLR (R for loop being recorded) and we reach the header

algorithm.

of a different loopLO (O for other loop). Note thatLO must be an

The key observation is that when an outer trace aborts because

inner loop ofLR because we stop the trace when we exit a loop.

the inner tree is not ready, this is probably a temporary condition.

• IfLO has a type-matching compiled trace tree, we callLO as Thus, we should not count such aborts toward blacklisting as long

a nested trace tree. If the call succeeds, then we record the callas we are able to build up more traces for the inner tree.

in the trace forLR . On future executions, the trace forLR will In our implementation, when an outer tree aborts on the inner

call the inner trace directly.

tree, we increment the outer tree’s blacklist counter as usual and

back off on compiling it. When the inner tree finishes a trace, we

• IfLO does not have a type-matching compiled trace tree yet,

we have to obtain it before we are able to proceed. In orderdecrement the blacklist counter on the outer loop, “forgiving” the

to do this, we simply abort recording the first trace. The traceouter loop for aborting previously. We also undo the backoff so that

monitor will see the inner loop header, and will immediatelythe outer tree can start immediately trying to compile the next time

start recording the inner loop.2

we reach it.

If all the loops in a nest are type-stable, then loop nesting creates

no duplication. Otherwise, if loops are nested to a depthk, and each 5. Trace Tree Optimization

This section explains how a recorded trace is translated to an

2Instead of aborting the outer recording, we could principally merely sus-

pend the recording, but that would require the implementation to be ableoptimized machine code trace. The trace compilation subsystem,

to record several traces simultaneously, complicating the implementation,NANOJIT , is separate from the VM and can be used for other

applications.

while saving only a few iterations in the interpreter.


Tag JS Type Description

5.1 Optimizations

xx1 number 31-bit integer representation

Because traces are in SSA form and have no join points orφ-

000 object pointer to JSObject handle

nodes, certain optimizations are easy to implement. In order to

010 number pointer to double handle

get good startup performance, the optimizations must run quickly,

100 string pointer to JSString handle

so we chose a small set of optimizations. We implemented the

110 boolean enumeration for null, undefined, true, false

optimizations as pipelined filters so that they can be turned on and

null, or

off independently, and yet all run in just two loop passes over the

undefined

trace: one forward and one backward.

Every time the trace recorder emits a LIR instruction, the in-Figure 9. Tagged values in the SpiderMonkey JS interpreter.

struction is immediately passed to the first filter in the forwardTesting tags, unboxing (extracting the untagged value) and boxing

pipeline. Thus, forward filter optimizations are performed as the(creating tagged values) are significant costs. Avoiding these costs

trace is recorded. Each filter may pass each instruction to the nextis a key benefit of tracing.

filter unchanged, write a different instruction to the next filter, or

write no instruction at all. For example, the constant folding filter

can replace a multiply instruction likev13 := mul3,1000with a

constant instructionv13 = 3000.

heuristic selectsv with minimumvm . The motivation is that this

We currently apply four forward filters:

frees up a register for as long as possible given a single spill.

If we need to spill a valuevs at this point, we generate the

• On ISAs without floating-point instructions, a soft-float filterrestore code just after the code for the current instruction. The

converts floating-point LIR instructions to sequences of integercorresponding spill code is generated just after the last point where

instructions.

vs was used. The register that was assigned tovs is marked free for

the preceding code, because that register can now be used freely

• CSE (constant subexpression elimination),

without affecting the following code

• expression simplification, including constant folding and a few

algebraic identities (e.g.,a� a= 0), and

### 6. Implementation

• source language semantic-specific expression simplification,

To demonstrate the effectiveness of our approach, we have im-

primarily algebraic identities that allowDOUBLE to be replaced

plemented a trace-based dynamic compiler for the SpiderMonkey

withINT. For example, LIR that converts anINT to aDOUBLE

and then back again would be removed by this filter.JavaScript Virtual Machine (4). SpiderMonkey is the JavaScript

VM embedded in Mozilla’s Firefox open-source web browser (2),

When trace recording is completed, nanojit runs the backwardwhich is used by more than 200 million users world-wide. The core

optimization filters. These are used for optimizations that requireof SpiderMonkey is a bytecode interpreter implemented in C++.

backward program analysis. When running the backward filters,In SpiderMonkey, all JavaScript values are represented by the

nanojit reads one LIR instruction at a time, and the reads are passedtypejsval. Ajsvalis machine word in which up to the 3 of the

through the pipeline.

least significant bits are a type tag, and the remaining bits are data.

We currently apply three backward filters:

See Figure 6 for details. All pointers contained injsvalspoint to

GC-controlled blocks aligned on 8-byte boundaries.

• Dead data-stack store elimination. The LIR trace encodes many

JavaScriptobjectvalues are mappings of string-valued property

stores to locations in the interpreter stack. But these values are

names to arbitrary values. They are represented in one of two ways

never read back before exiting the trace (by the interpreter or

in SpiderMonkey. Most objects are represented by a shared struc-

another trace). Thus, stores to the stack that are overwritten

tural description, called theobject shape, that maps property names

before the next exit are dead. Stores to locations that are off

to array indexes using a hash table. The object stores a pointer to

the top of the interpreter stack at future exits are also dead.

the shape and the array of its own property values. Objects with

• Dead call-stack store elimination. This is the same optimizationlarge, unique sets of property names store their properties directly

as above, except applied to the interpreter’s call stack used forin a hash table.

function call inlining.

The garbage collector is an exact, non-generational, stop-the-

• Dead code elimination. This eliminates any operation thatworld mark-and-sweep collector.

In the rest of this section we discuss key areas of the TraceMon-

stores to a value that is never used.

key implementation.

After a LIR instruction is successfully read (“pulled”) from

the backward filter pipeline, nanojit’s code generator emits native6.1 Calling Compiled Traces

machine instruction(s) for it.

Compiled traces are stored in atrace cache, indexed by intepreter

PC and type map. Traces are compiled so that they may be

5.2 Register Allocation

called as functions using standard native calling conventions (e.g.,

We use a simple greedy register allocator that makes a singleFASTCALLon x86).

backward pass over the trace (it is integrated with the code gen-The interpreter must hit a loop edge and enter the monitor in

erator). By the time the allocator has reached an instruction likeorder to call a native trace for the first time. The monitor computes

v3 = add v1, v2, it has already assigned a register tov3. Ifv1 and the current type map, checks the trace cache for a trace for the

v2 have not yet been assigned registers, the allocator assigns a freecurrent PC and type map, and if it finds one, executes the trace.

register to each. If there are no free registers, a value is selected forTo execute a trace, the monitor must build a trace activation

spilling. We use a class heuristic that selects the “oldest” register-record containing imported local and global variables, temporary

carried value (6).

stack space, and space for arguments to native calls. The local and

The heuristic considers the setR of valuesvin registers imme-global values are then copied from the interpreter state to the trace

diately after the current instruction for spilling. Letvm be the last activation record. Then, the trace is called like a normal C function

instruction before the current where eachvis referred to. Then thepointer.


When a trace call returns, the monitor restores the interpreterRecording is activated by a pointer swap that sets the inter-

state. First, the monitor checks the reason for the trace exit andpreter’s dispatch table to call a single “interrupt” routine for ev-

applies blacklisting if needed. Then, it pops or synthesizes inter-ery bytecode. The interrupt routine first calls a bytecode-specific

preter JavaScript call stack frames as needed. Finally, it copies therecording routine. Then, it turns off recording if necessary (e.g.,

imported variables back from the trace activation record to the in-the trace ended). Finally, it jumps to the standard interpreter byte-

code implementation. Some bytecodes have effects on the type map

terpreter state.

At least in the current implementation, these steps have a non-that cannot be predicted before executing the bytecode (e.g., call-

negligible runtime cost, so minimizing the number of interpreter-ingString.charCodeAt, which returns an integer orNaN if the

to-trace and trace-to-interpreter transitions is essential for perfor-index argument is out of range). For these, we arrange for the inter-

mance. (see also Section 3.3). Our experiments (see Figure 12)preter to call into the recorder again after executing the bytecode.

show that for programs we can trace well such transitions hap-Since such hooks are relatively rare, we embed them directly into

pen infrequently and hence do not contribute significantly to totalthe interpreter, with an additional runtime check to see whether a

runtime. In a few programs, where the system is prevented fromrecorder is currently active.

recording branch traces for hot side exits by aborts, this cost canWhile separating the interpreter from the recorder reduces indi-

vidual code complexity, it also requires careful implementation and

rise to up to 10% of total execution time.

extensive testing to achieve semantic equivalence.

6.2 Trace Stitching

In some cases achieving this equivalence is difficult since Spi-

Transitions from a trace to a branch trace at a side exit avoid thederMonkey follows afat-bytecodedesign, which was found to be

costs of calling traces from the monitor, in a feature calledtrace beneficial to pure interpreter performance.

In fat-bytecode designs, individual bytecodes can implement

stitching. At a side exit, the exiting trace only needs to write live

register-carried values back to its trace activation record. In our im-complex processing (e.g., thegetpropbytecode, which imple-

plementation, identical type maps yield identical activation recordments full JavaScript property value access, including special cases

layouts, so the trace activation record can be reused immediatelyfor cached and dense array access).

Fat bytecodes have two advantages: fewer bytecodes means

by the branch trace.

In programs with branchy trace trees with small traces, tracelower dispatch cost, and bigger bytecode implementations give the

stitching has a noticeable cost. Although writing to memory andcompiler more opportunities to optimize the interpreter.

then soon reading back would be expected to have a high L1Fat bytecodes are a problem for TraceMonkey because they

cache hit rate, for small traces the increased instruction count hasrequire the recorder to reimplement the same special case logic

a noticeable cost. Also, if the writes and reads are very closein the same way. Also, the advantages are reduced because (a)

in the dynamic instruction stream, we have found that currentdispatch costs are eliminated entirely in compiled traces, (b) the

x86 processors often incur penalties of 6 cycles or more (e.g., iftraces contain only one special case, not the interpreter’s large

the instructions use different base registers with equal values, thechunk of code, and (c) TraceMonkey spends less time running the

processor may not be able to detect that the addresses are the samebase interpreter.

One way we have mitigated these problems is by implementing

right away).

The alternate solution is to recompile an entire trace tree, thuscertain complex bytecodes in the recorder as sequences of simple

achieving inter-trace register allocation (10). The disadvantage isbytecodes. Expressing the original semantics this way is not too dif-

that tree recompilation takes time quadratic in the number of traces.ficult, and recording simple bytecodes is much easier. This enables

We believe that the cost of recompiling a trace tree every timeus to retain the advantages of fat bytecodes while avoiding some of

a branch is added would be prohibitive. That problem might betheir problems for trace recording. This is particularly effective for

mitigated by recompiling only at certain points, or only for veryfat bytecodes that recurse back into the interpreter, for example to

convert an object into a primitive value by invoking a well-known

hot, stable trees.

In the future, multicore hardware is expected to be common,method on the object, since it lets us inline this function call.

making background tree recompilation attractive. In a closely re-It is important to note that we split fat opcodes into thinner op-

lated project (13) background recompilation yielded speedups ofcodes only during recording. When running purely interpretatively

up to 1.25x on benchmarks with many branch traces. We plan to(i.e. code that has been blacklisted), the interpreter directly and ef-

apply this technique to TraceMonkey as future work. ficiently executes the fat opcodes.

6.3 Trace Recording

The job of the trace recorder is to emit LIR with identical semantics6.4 Preemption

to the currently running interpreter bytecode trace. A good imple-SpiderMonkey, like many VMs, needs to preempt the user program

mentation should have low impact on non-tracing interpreter per-periodically. The main reasons are to prevent infinitely looping

formance and a convenient way for implementers to maintain se-scripts from locking up the host system and to schedule GC.

In the interpreter, this had been implemented by setting a “pre-

mantic equivalence.

In our implementation, the only direct modification to the inter-empt now” flag that was checked on every backward jump. This

preter is a call to the trace monitor at loop edges. In our benchmarkstrategy carried over into TraceMonkey: the VM inserts a guard on

results (see Figure 12) the total time spent in the monitor (for allthe preemption flag at every loop edge. We measured less than a

activities) is usually less than 5%, so we consider the interpreter1% increase in runtime on most benchmarks for this extra guard.

impact requirement met. Incrementing the loop hit counter is ex-In practice, the cost is detectable only for programs with very short

pensive because it requires us to look up the loop in the trace cache,loops.

but we have tuned our loops to become hot and trace very quicklyWe tested and rejected a solution that avoided the guards by

(on the second iteration). The hit counter implementation could becompiling the loop edge as an unconditional jump, and patching

improved, which might give us a small increase in overall perfor-the jump target to an exit routine when preemption is required.

mance, as well as more flexibility with tuning hotness thresholds.This solution can make the normal case slightly faster, but then

Once a loop is blacklisted we never call into the trace monitor forpreemption becomes very slow. The implementation was also very

complex, especially trying to restart execution after the preemption.

that loop (see Section 3.3).


6.5 Calling External Functions

?>9@AJ.D<F@-<>2.@A:0>#3$4,56#

Like most interpreters, SpiderMonkey has a foreign function inter-?>9@AJ.0A:</C./8-2#3$4%56#

?>9@AJ.><J/F80-#3$4$56#

face (FFI) that allows it to call C builtins and host system functions?>9@AJ.B<?><#3$4(56#

(e.g., web browser control and DOM access). The FFI has a stan-?>9@AJ.1<?2)'#3%4(56#

92J25:.-A<#3'4%56#

dard signature for JS-callable functions, the key argument of which7<>;.?:2/>9<F.A897#3*4$56#

is an array of boxed values. External functions called through the7<>;.:<9I<F.?07?#3(4,56#

7<>;./89-@/#3'4,56#

FFI interact with the program state through an interpreter API (e.g.,-<>2.B897<>.5:<91#3$4!56#

to read a property from an argument). There are also certain inter--<>2.B897<>.>8H2#3$4$56#/9=:>8.?;<$#3(4,56#

preter builtins that do not use the FFI, but interact with the program/9=:>8.7-(#3%4&56#

/9=:>8.<2?#3$4)56#

state in the same way, such as theCallIteratorNextfunction

/8A>98FG8E.92/09?@D2#3$4!56#

used with iterator objects. TraceMonkey must support this FFI in1@>8:?.A?@2D2.1@>?#3%4*56#

order to speed up code that interacts with the host system inside hot1@>8:?.1@>E@?2.<A-#3%(4%56#1@>8:?.1@>?.@A.1=>2#3+4*56#

loops.

1@>8:?.&1@>.1@>?.@A.1=>2#3%(4(56#

Calling external functions from TraceMonkey is potentially dif-<//2??.A?@2D2#3&4!56#<//2??.A18-=#3'4%56#

ficult because traces do not update the interpreter state until exit-<//2??.B<AAC0/;#3%4%56#

ing. In particular, external functions may need the call stack or the<//2??.1@A<9=.>922?#3!4,56#&-.9<=>9</2#3$4%56#

global variables, but they may be out of date.

&-.789:;#3%4,56#

For the out-of-date call stack problem, we refactored some of&-./012#3%4%56#

the interpreter API implementation functions to re-materialize the!"# $!"# %!"# &!"# '!"# (!"# )!"# *!"# +!"# ,!"# $!!"#

interpreter call stack on demand.

KA>29:92>#

L<ID2#

We developed a C++ static analysis and annotated some inter-

preter functions in order to verify that the call stack is refreshed

at any point it needs to be used. In order to access the call stack,

Figure 11. Fraction of dynamic bytecodes executed by inter-

a function must be annotated as either FORCES STACK or RE-

preter and on native traces.The speedup vs. interpreter is shown

QUIRES STACK

. These annotations are also required in order to call

in parentheses next to each test. The fraction of bytecodes exe-

REQUIRES TACK

S

functions, which are presumed to access the call

cuted while recording is too small to see in this figure, except

stack transitively. FORCES STACK

is a trusted annotation, applied

forcrypto-md5, where fully 3% of bytecodes are executed while

to only 5 functions, that means the function refreshes the call stack.

recording. In most of the tests, almost all the bytecodes are exe-

REQUIRES TACK

S

is an untrusted annotation that means the func-

cuted by compiled traces. Three of the benchmarks are not traced

tion may only be called if the call stack has already been refreshed.

at all and run in the interpreter.

Similarly, we detect when host functions attempt to directly

read or write global variables, and force the currently running trace

to side exit. This is necessary since we cache and unbox globalloops and heavily branching code, and a specialized fuzz tester in-

variables into the activation record during trace execution.deed revealed several regressions which we subsequently corrected.

Since both call-stack access and global variable access are

rarely performed by host functions, performance is not significantly

### 7. Evaluation

affected by these safety mechanisms.

Another problem is that external functions can reenter the inter-We evaluated our JavaScript tracing implementation using Sun-

preter by calling scripts, which in turn again might want to accessSpider, the industry standard JavaScript benchmark suite. SunSpi-

the call stack or global variables. To address this problem, we madeder consists of 26 short-running (less than 250ms, average 26ms)

the VM set a flag whenever the interpreter is reentered while a com-JavaScript programs. This is in stark contrast to benchmark suites

piled trace is running.

such as SpecJVM98 (3) used to evaluate desktop and server Java

Every call to an external function then checks this flag and exitsVMs. Many programs in those benchmarks use large data sets and

the trace immediately after returning from the external function callexecute for minutes. The SunSpider programs carry out a variety of

if it is set. There are many external functions that seldom or nevertasks, primarily 3d rendering, bit-bashing, cryptographic encoding,

reenter, and they can be called without problem, and will causemath kernels, and string processing.

trace exit only if necessary.

All experiments were performed on a MacBook Pro with 2.2

The FFI’s boxed value array requirement has a performanceGHz Core 2 processor and 2 GB RAM running MacOS 10.5.

cost, so we defined a new FFI that allows C functions to be an-Benchmark results.The main question is whether programs

notated with their argument types so that the tracer can call themrun faster with tracing. For this, we ran the standard SunSpider test

directly, without unnecessary argument conversions. driver, which starts a JavaScript interpreter, loads and runs each

Currently, we do not support calling native property get and setprogram once for warmup, then loads and runs each program 10

override functions or DOM functions directly from trace. Supporttimes and reports the average time taken by each. We ran 4 differ-

is planned future work.

ent configurations for comparison: (a) SpiderMonkey, the baseline

interpreter, (b) TraceMonkey, (d) SquirrelFish Extreme (SFX), the

6.6 Correctness

call-threaded JavaScript interpreter used in Apple’s WebKit, and

During development, we had access to existing JavaScript test(e) V8, the method-compiling JavaScript VM from Google.

suites, but most of them were not designed with tracing VMs inFigure 10 shows the relative speedups achieved by tracing, SFX,

mind and contained few loops.

and V8 against the baseline (SpiderMonkey). Tracing achieves the

One tool that helped us greatly was Mozilla’s JavaScript fuzzbest speedups in integer-heavy benchmarks, up to the 25x speedup

tester,JSFUNFUZZ

, which generates random JavaScript programsonbitops-bitwise-and.

by nesting random language elements. We modifiedJSFUNFUZZ

TraceMonkey is the fastest VM on 9 of the 26 benchmarks

to generate loops, and also to test more heavily certain constructs(3d-morph,bitops-3bit-bits-in-byte,bitops-bitwise-

we suspected would reveal flaws in our implementation. For exam-and,crypto-sha1,math-cordic,math-partial-sums,math-

ple, we suspected bugs in TraceMonkey’s handling of type-unstablespectral-norm,string-base64,string-validate-input).


%#"

%!"

$#"

&'()*+,"

-./"

01"

$!"

#"

!"

<!-- empty unresolved VLM region -->

Figure 10.Speedup vs. a baseline JavaScript interpreter (SpiderMonkey) for our trace-based JIT compiler, Apple’s SquirrelFish Extreme

inline threading interpreter and Google’s V8 JS compiler. Our system generates particularly efficient code for programs that benefit most from

type specialization, which includes SunSpider Benchmark programs that perform bit manipulation. We type-specialize the code in question

to use integer arithmetic, which substantially improves performance. For one of the benchmark programs we execute 25 times faster than

the SpiderMonkey interpreter, and almost 5 times faster than V8 and SFX. For a large number of benchmarks all three VMs produce similar

results. We perform worst on benchmark programs that we do not trace and instead fall back onto the interpreter. This includes the recursive

benchmarksaccess-binary-treesandcontrol-flow-recursive, for which we currently don’t generate any native code.

• Two programs trace well, but have a long compilation time.

In particular, thebitopsbenchmarks are short programs that per-

form many bitwise operations, so TraceMonkey can cover the en-access-nbodyforms a large number of traces (81).crypto-md5

tire program with 1 or 2 traces that operate on integers. TraceMon-forms one very long trace. We expect to improve performance

key runs all the other programs in this set almost entirely as nativeon this programs by improving the compilation speed of nano-

jit.

code.

regexp-dnais dominated by regular expression matching,• Some programs trace very well, and speed up compared to

which is implemented in all 3 VMs by a special regular expressionthe interpreter, but are not as fast as SFX and/or V8, namely

compiler. Thus, performance on this benchmark has little relationbitops-bits-in-byte, bitops-nsieve-bits, access-

to the trace compilation approach discussed in this paper.fannkuch,access-nsieve, andcrypto-aes. The reason is

TraceMonkey’s smaller speedups on the other benchmarks cannot clear, but all of these programs have nested loops with

be attributed to a few specific causes:

small bodies, so we suspect that the implementation has a rela-

tively high cost for calling nested traces.string-fastatraces

• The implementation does not currently trace recursion, sowell, but its run time is dominated by string processing builtins,

TraceMonkey achieves a small speedup or no speedup onwhich are unaffected by tracing and seem to be less efficient in

SpiderMonkey than in the two other VMs.

benchmarks that use recursion extensively:3d-cube,3d-

raytrace,access-binary-trees,string-tagcloud, and

Detailed performance metrics.In Figure 11 we show the frac-

controlflow-recursive.

tion of instructions interpreted and the fraction of instructions exe-

• The implementation does not currently traceevaland some

cuted as native code. This figure shows that for many programs, we

other functions implemented in C. Becausedate-format-

are able to execute almost all the code natively.

tofteanddate-format-xparbuse such functions in theirFigure 12 breaks down the total execution time into four activ-

main loops, we do not trace them.

ities: interpreting bytecodes while not recording, recording traces

• The implementation does not currently trace through regular(including time taken to interpret the recorded trace), compiling

traces to native code, and executing native code traces.

expressionreplaceoperations. The replace function can be

passed a function object used to compute the replacement text.These detailed metrics allow us to estimate parameters for a

Our implementation currently does not trace functions calledsimple model of tracing performance. These estimates should be

considered very rough, as the values observed on the individual

as replace functions. The run time ofstring-unpack-codeis

benchmarks have large standard deviations (on the order of the

dominated by such areplacecall.


Loops Trees Traces Aborts Flushes Trees/Loop Traces/Tree Traces/Loop Speedup

3d-cube 25 27 29 3 0 1.1 1.1 1.2 2.20x

3d-morph 5 8 8 2 0 1.6 1.0 1.6 2.86x

3d-raytrace 10 25 100 10 1 2.5 4.0 10.0 1.18x

access-binary-trees 0 0 0 5 0 - - - 0.93x

access-fannkuch 10 34 57 24 0 3.4 1.7 5.7 2.20x

access-nbody 8 16 18 5 0 2.0 1.1 2.3 4.19x

access-nsieve 3 6 8 3 0 2.0 1.3 2.7 3.05x

bitops-3bit-bits-in-byte 2 2 2 0 0 1.0 1.0 1.0 25.47x

bitops-bits-in-byte 3 3 4 1 0 1.0 1.3 1.3 8.67x

bitops-bitwise-and 1 1 1 0 0 1.0 1.0 1.0 25.20x

bitops-nsieve-bits 3 3 5 0 0 1.0 1.7 1.7 2.75x

controlflow-recursive 0 0 0 1 0 - - - 0.98x

crypto-aes 50 72 78 19 0 1.4 1.1 1.6 1.64x

crypto-md5 4 4 5 0 0 1.0 1.3 1.3 2.30x

crypto-sha1 5 5 10 0 0 1.0 2.0 2.0 5.95x

date-format-tofte 3 3 4 7 0 1.0 1.3 1.3 1.07x

date-format-xparb 3 3 11 3 0 1.0 3.7 3.7 0.98x

math-cordic 2 4 5 1 0 2.0 1.3 2.5 4.92x

math-partial-sums 2 4 4 1 0 2.0 1.0 2.0 5.90x

math-spectral-norm 15 20 20 0 0 1.3 1.0 1.3 7.12x

regexp-dna 2 2 2 0 0 1.0 1.0 1.0 4.21x

string-base64 3 5 7 0 0 1.7 1.4 2.3 2.53x

string-fasta 5 11 15 6 0 2.2 1.4 3.0 1.49x

string-tagcloud 3 6 6 5 0 2.0 1.0 2.0 1.09x

string-unpack-code 4 4 37 0 0 1.0 9.3 9.3 1.20x

string-validate-input 6 10 13 1 0 1.7 1.3 2.2 1.86x

Figure 13.Detailed trace recording statistics for the SunSpider benchmark set.

fastest available JavaScript inline threaded interpreter (SFX) on 9

mean). We excluderegexp-dnafrom the following calculations,

because most of its time is spent in the regular expression matcher,of 26 benchmarks.

which has much different performance characteristics from the

other programs. (Note that this only makes a difference of about

10% in the results.) Dividing the total execution time in processor

clock cycles by the number of bytecodes executed in the base8. Related Work

interpreter shows that on average, a bytecode executes in aboutTrace optimization for dynamic languages.The closest area of

35 cycles. Native traces take about 9 cycles per bytecode, a 3.9xrelated work is on applying trace optimization to type-specialize

speedup over the interpreter.

dynamic languages. Existing work shares the idea of generating

Using similar computations, we find that trace recording takestype-specialized code speculatively with guards along interpreter

about 3800 cycles per bytecode, and compilation 3150 cycles pertraces.

bytecode. Hence, during recording and compiling the VM runs atTo our knowledge, Rigo’s Psyco (16) is the only published

1/200 the speed of the interpreter. Because it costs 6950 cycles totype-specializing trace compiler for a dynamic language (Python).

compile a bytecode, and we save 26 cycles each time that code isPsyco does not attempt to identify hot loops or inline function calls.

run natively, we break even after running a trace 270 times.Instead, Psyco transforms loops to mutual recursion before running

The other VMs we compared with achieve an overall speedupand traces all operations.

of 3.0x relative to our baseline interpreter. Our estimated nativePall’s LuaJIT is a Lua VM in development that uses trace com-

code speedup of 3.9x is significantly better. This suggests thatpilation ideas. (1). There are no publications on LuaJIT but the cre-

our compilation techniques can generate more efficient native codeator has told us that LuaJIT has a similar design to our system, but

than any other current JavaScript VM.

will use a less aggressive type speculation (e.g., using a floating-

These estimates also indicate that our startup performance couldpoint representation for all number values) and does not generate

be substantially better if we improved the speed of trace recordingnested traces for nested loops.

and compilation. The estimated 200x slowdown for recording andGeneral trace optimization.General trace optimization has

compilation is very rough, and may be influenced by startup factorsa longer history that has treated mostly native code and typed

in the interpreter (e.g., caches that have not warmed up yet duringlanguages like Java. Thus, these systems have focused less on type

recording). One observation supporting this conjecture is that inspecialization and more on other optimizations.

the tracer, interpreted bytecodes take about 180 cycles to run. Still,Dynamo (7) by Bala et al, introduced native code tracing as a

recording and compilation are clearly both expensive, and a betterreplacement for profile-guided optimization (PGO). A major goal

implementation, possibly including redesign of the LIR abstractwas to perform PGO online so that the profile was specific to

syntax or encoding, would improve startup performance.the current execution. Dynamo used loop headers as candidate hot

Our performance results confirm that type specialization usingtraces, but did not try to create loop traces specifically.

trace trees substantially improves performance. We are able toTrace trees were originally proposed by Gal et al. (11) in the

outperform the fastest available JavaScript compiler (V8) and thecontext of Java, a statically typed language. Their trace trees ac-

tually inlined parts of outer loops within the inner loops (because


erate native code with nearly the same structure but better perfor-

=<6>?J+B:F>*:</+>?7-<#0(1923#

mance.

=<6>?J+-?7:,A+,5*/#0(1$23#

Call threading, also known as context threading (8), compiles

=<6>?J+<:J,F5-*#0(1(23#

=<6>?J+@:=<:#0(1C23#

methods by generating a native call instruction to an interpreter

=<6>?J+.:=/&%#0$1C23#

method for each interpreter bytecode. A call-return pair has been

6/J/27+*?:#0%1$23#

4:<8+=7/,<6:F+?564#0D1(23#

shown to be a potentially much more efficient dispatch mechanism

4:<8+7:6I:F+=-4=#0C1923#

than the indirect jumps used in standard bytecode interpreters.

4:<8+,56*>,#0%1923#

*:</+@564:<+27:6.#0(1!23#

Inline threading (15) copies chunks of interpreter native code

*:</+@564:<+<5H/#0(1(23#

which implement the required bytecodes into a native code cache,

,6;7<5+=8:(#0C1923#

,6;7<5+4*C#0$1)23#

thus acting as a simple per-method JIT compiler that eliminates the

,6;7<5+:/=#0(1&23#

dispatch overhead.

,5?<65FG5E+6/,-6=>B/#0(1!23#

.><57=+?=>/B/+.><=#0$1D23#

Neither call threading nor inline threading perform type special-

.><57=+.><E>=/+:?*#0$C1$23#

ization.

.><57=+.><=+>?+.;</#0'1D23#

.><57=+).><+.><=+>?+.;</#0$C1C23#

Apple’s SquirrelFish Extreme (5) is a JavaScript implementa-

:,,/==+?=>/B/#0)1!23#

:,,/==+?.5*;#0%1$23#

tion based on call threading with selective inline threading. Com-

:,,/==+@:??A-,8#0$1$23#

bined with efficient interpreter engineering, these threading tech-

:,,/==+.>?:6;+<6//=#0!1923#

)*+6:;<6:,/#0(1$23#

niques have given SFX excellent performance on the standard Sun-

)*+45678#0$1923#

Spider benchmarks.

)*+,-./#0$1$23#

Google’s V8 is a JavaScript implementation primarily based

!"#

$!"#

%!"#

&!"#

'!"#

(!!"#

on inline threading, with call threading only for very complex

K?</676/<# L5?><56# M/,56*# N547>F/# N:FF#O6:,/#M-?#O6:,/#

operations.

### 9. Conclusions

Figure 12. Fraction of time spent on major VM activities.The

speedup vs. interpreter is shown in parentheses next to each test.This paper described how to run dynamic languages efficiently by

Most programs where the VM spends the majority of its time run-recording hot traces and generating type-specialized native code.

ning native code have a good speedup. Recording and compilationOur technique focuses on aggressively inlined loops, and for each

costs can be substantial; speeding up those parts of the implemen-loop, it generates a tree of native code traces representing the

paths and value types through the loop observed at run time. We

tation would improve SunSpider performance.

explained how to identify loop nesting relationships and generate

nested traces in order to avoid excessive code duplication due

to the many paths through a loop nest. We described our type

specialization algorithm. We also described our trace compiler,

inner loops become hot first), leading to much greater tail duplica-which translates a trace from an intermediate representation to

optimized native code in two linear passes.

tion.

YETI, from Zaleski et al. (19) applied Dynamo-style tracingOur experimental results show that in practice loops typically

to Java in order to achieve inlining, indirect jump elimination,are entered with only a few different combinations of value types

and other optimizations. Their primary focus was on designing anof variables. Thus, a small number of traces per loop is sufficient

interpreter that could easily be gradually re-engineered as a tracingto run a program efficiently. Our experiments also show that on

programs amenable to tracing, we achieve speedups of 2x to 20x.

VM.

Suganuma et al. (18) described region-based compilation (RBC),

a relative of tracing. A region is an subprogram worth optimizing10. Future Work

that can include subsets of any number of methods. Thus, the com-

Work is underway in a number of areas to further improve the

piler has more flexibility and can potentially generate better code,

performance of our trace-based JavaScript compiler. We currently

but the profiling and compilation systems are correspondingly more

do not trace across recursive function calls, but plan to add the

complex.

support for this capability in the near term. We are also exploring

Type specialization for dynamic languages.Dynamic lan-

adoption of the existing work on tree recompilation in the context

guage implementors have long recognized the importance of type

of the presented dynamic compiler in order to minimize JIT pause

specialization for performance. Most previous work has focused on

times and obtain the best of both worlds, fast tree stitching as well

methods instead of traces.

as the improved code quality due to tree recompilation.

Chambers et. al (9) pioneered the idea of compiling multiple

We also plan on adding support for tracing across regular ex-

versions of a procedure specialized for the input types in the lan-

pression substitutions using lambda functions, function applica-

guage Self. In one implementation, they generated a specialized

tions and expression evaluation usingeval. All these language

method online each time a method was called with new input types.

constructs are currently executed via interpretation, which limits

In another, they used an offline whole-program static analysis to

our performance for applications that use those features.

infer input types and constant receiver types at call sites. Interest-

ingly, the two techniques produced nearly the same performance.Salib (17) designed a type inference algorithm for Python basedAcknowledgments

on the Cartesian Product Algorithm and used the results to special-Parts of this effort have been sponsored by the National Science

Foundation under grants CNS-0615443 and CNS-0627747, as well

ize on types and translate the program to C++.

McCloskey (14) has work in progress based on a language-as by the California MICRO Program and industrial sponsor Sun

independent type inference that is used to generate efficient CMicrosystems under Project No. 07-127.

The U.S. Government is authorized to reproduce and distribute

implementations of JavaScript and Python programs.

reprints for Governmental purposes notwithstanding any copyright

Native code generation by interpreters.The traditional inter-

preter design is a virtual machine that directly executes ASTs orannotation thereon. Any opinions, findings, and conclusions or rec-

machine-code-like bytecodes. Researchers have shown how to gen-ommendations expressed here are those of the author and should


not be interpreted as necessarily representing the official views,[10] A. Gal.Efficient Bytecode Verification and Compilation in a Virtual

policies or endorsements, either expressed or implied, of the Na-Machine Dissertation. PhD thesis, University Of California, Irvine,

tional Science foundation (NSF), any other agency of the U.S. Gov-2006.

ernment, or any of the companies mentioned above.

[11] A. Gal, C. W. Probst, and M. Franz. HotpathVM: An effective JIT

compiler for resource-constrained devices. InProceedings of the

## References

International Conference on Virtual Execution Environments, pages

144–153. ACM Press, 2006.

[1] LuaJIT roadmap 2008 - http://lua-users.org/lists/lua-l/2008-

[12] C. Garrett, J. Dean, D. Grove, and C. Chambers. Measurement and

02/msg00051.html.

Application of Dynamic Receiver Class Distributions. 1994.

[2] Mozilla — Firefox web browser and Thunderbird email client -

[13] J. Ha, M. R. Haghighat, S. Cong, and K. S. McKinley. A concurrent

http://www.mozilla.com.

trace-based just-in-time compiler for javascript. Dept.of Computer

[3] SPECJVM98 - http://www.spec.org/jvm98/.

Sciences, The University of Texas at Austin, TR-09-06, 2009.

[4] SpiderMonkey (JavaScript-C) Engine -

[14] B. McCloskey. Personal communication.

http://www.mozilla.org/js/spidermonkey/.

[15] I. Piumarta and F. Riccardi. Optimizing direct threaded code by selec-

[5] Surfin’ Safari - Blog Archive - Announcing SquirrelFish Extreme -tive inlining. InProceedings of the ACM SIGPLAN 1998 conference

http://webkit.org/blog/214/introducing-squirrelfish-extreme/.on Programming language design and implementation, pages 291–

[6] A. Aho, R. Sethi, J. Ullman, and M. Lam. Compilers: Principles,300. ACM New York, NY, USA, 1998.

techniques, and tools, 2006.

[16] A. Rigo. Representation-Based Just-In-time Specialization and the

[7] V. Bala, E. Duesterwald, and S. Banerjia. Dynamo: A transparentPsyco Prototype for Python. InPEPM , 2004.

dynamic optimization system. InProceedings of the ACM SIGPLAN [17] M. Salib. Starkiller: A Static Type Inferencer and Compiler for

Conference on Programming Language Design and Implementation, Python. InMaster’s Thesis, 2004.

pages 1–12. ACM Press, 2000.

[18] T. Suganuma, T. Yasue, and T. Nakatani. A Region-Based Compila-

[8] M. Berndl, B. Vitale, M. Zaleski, and A. Brown. Context Threading:

tion Technique for Dynamic Compilers.ACM Transactions on Pro-

a Flexible and Efficient Dispatch Technique for Virtual Machine In-

gramming Languages and Systems (TOPLAS), 28(1):134–174, 2006.

terpreters. InCode Generation and Optimization, 2005. CGO 2005.

[19] M. Zaleski, A. D. Brown, and K. Stoodley. YETI: A graduallY

International Symposium on, pages 15–26, 2005.

Extensible Trace Interpreter. InProceedings of the International

[9] C. Chambers and D. Ungar. Customization: Optimizing Compiler

Conference on Virtual Execution Environments, pages 83–93. ACM

Technology for SELF, a Dynamically-Typed O bject-Oriented Pro-

Press, 2007.

gramming Language. InProceedings of the ACM SIGPLAN 1989

Conference on Programming Language Design and Implementation,

pages 146–160. ACM New York, NY, USA, 1989.

