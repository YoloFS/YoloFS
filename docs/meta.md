# Use agents across the full research lifecycle

This article describes how agents can help across the full research
lifecycle: literature review, empirical study, design,
implementation, evaluation, and writing.

**Role of the researcher.** Find the problem. Frame the question. Make
judgment calls on the design. Influence implementation taste. Shape the
narrative arc. Build the harness and write the rules for agents.
Advise the agent to be an expert in the field. Understand and verify
the agent output. Take responsibility for the work. The researcher is in the loop the
entire time.

**Role of the agent.** Search and summarize prior art. Answer questions
about existing knowledge. Discuss with the researcher on design and
plans. Implement the design and review code.
Write tests and debug. Run experiments and analyze results. The agent
makes mistakes, so its output still needs the researcher to understand,
verify, and correct.

**What this means for research.** Credit may no longer go to how
complex the system is, how many lines of code it has, or how large the
scale is. What matters more is the *importance* of the problem
(usefulness) and the *ingenuity* of the solution (novelty).

**Where agents fail.** Agents drift on long sessions, over-engineer
when given vague goals, and sound confident even when they are wrong.
The fix is not to trust them less, but to give them tighter scope,
shorter sessions, and outputs that can be checked.

---

## 1. Literature review

I use agents a lot for literature search, but with caution. I have
two patterns that work for me.

The first is a **targeted lookup** on a topic I already know
something about. In YoloFS I asked the agent about the implementation
details of Linux landlock and seccomp, and how btrfs and zfs implement
snapshots, mostly to compare with my own design. I had the background
already; I just wanted detail. I still verify: I read the source the
agent cites if available, otherwise I re-search online. This is faster
than searching from scratch.

The second is a **broad survey** of a field I am only partly familiar
with. In a recent project I wanted to know how different languages and
frameworks expose asynchrony. Instead of asking "tell me about async,"
I gave the agent the explicit matrix to cover: how `{C++, Python, Go,
Java, JavaScript}` each expose asynchrony. I asked it to write the
result to `docs/background/*.md` with references, and I checked whether
the result matched my expectation. When it did not, I went back and
did the research myself. I have found this to be a very effective way
to fill in gaps in my own knowledge.

**Takeaway.** Agents search and summarize fast. Ask things you can
verify. Checking the answer is itself a good way to learn the
material.

---

## 2. Empirical study

Agents are very good at empirical study. One example is the misuse
study in YoloFS. We started with a Google Sheet of about 100
manually-collected and labeled reports of agent filesystem misuse.
From there I used agents in several stages, each producing output I
could verify.

**Data collection.** I gave the agent the existing URLs and a
methodology, and asked it to find similar reports. The set grew from
~100 to ~300. I refined the methodology as edge cases came up. Manual
review on every candidate kept the set clean, since it is usually
obvious whether a report belongs.

**Summarization.** I do not feed all reports into a single agent
context to summarize them, as it gives me no way to verify the
output. Instead, I wrote a script (with the agent) that opens a fresh
session per URL and produces a Markdown summary. One Markdown per URL
means the agent cannot cheat by skipping reports, and every number in
the paper traces back to a specific one. I can easily verify a summary
against its source, and agents turn out to be surprisingly good at
summarization.

**Classification.** The next pass takes each Markdown and produces a
YAML categorization. The taxonomy started from the original
hand-classified reports in the Google Sheet, and I refined it through
discussion with the agent. I then run a small seeding pass on a
handful of Markdowns to check that the rules hold up, and scale up to
all reports once the seeding pass is clean. A static
consistency-checking script catches mistakes across all reports:
label drift, missing fields, suspicious counts. If the rules need
adjusting later, I edit the taxonomy and have the agent re-classify
in minutes.

**Querying.** I asked the agent to build a table of contents over the
categorized results. What surprised me most is how useful the data set
became *afterwards*. With the table of contents and the per-report
Markdowns in place, the agent is unusually good at answering questions
about a particular report, or at finding all reports that satisfy some
condition. The empirical study became a queryable data set, not just a
one-shot survey. This paid off most during paper writing, when the agent could pull
examples and counter-examples out of the data set in seconds. Every answer traces back to a
specific report file, so I can open it and check.

**Takeaway.** Structure each stage's output so a human or a script
can verify it. Enforce static control flow over agents. Iterate on
the methodology, then scale. Index the data set so agents can query
it later.

---

## 3. Design

Design is one of the most important parts of research, and the part
where the human stays most in the loop to guide the direction and
make the architectural calls.

**Division of labor.** The architectural calls are mine: picking the
abstractions, the interface boundaries, the on-disk format, deciding
which alternative to pursue and which to reject. I often do this
thinking away from the laptop, on a walk, or in conversation with a
lab mate. At the laptop, the pull toward implementation is too
strong. It is too tempting to ask the agent something concrete and
let the work flow downhill into code. The agent can sketch
alternatives, draft design docs and refactor plans against a stated
goal, surface trade-offs, and cross-reference prior work.

**Design as docs.** For YoloFS, I put all the design docs in a
`docs/` directory covering architecture, staging, permission, and
internals, plus a `docs/plans/` directory of numbered, half-page
refactor plans. The plans are not something I write alone. I give the
agent a prompt to draft the plan, we iterate until we are both
satisfied, and for complex plans I ask multiple agents to review. I
also ask the agent for trade-offs and whether it sees a better
solution. These design docs pay off again during paper writing, where
they give the agent the architecture without making it
reverse-engineer from code.

**Example.** When I was designing the snapshot mechanism for
YoloFS, I was struggling to make many snapshots cheap without slowing
down regular I/O. I had been considering a multi-versioned state held
in kernel memory that could switch around during rollback, and it
kept getting more complex. After discussion with the agent, we
converged on a much simpler split: the kernel keeps only the latest
state, while userspace holds the full history and sends the state at
the chosen point back to the kernel on rollback. The agent did not
invent this on its own, but the back-and-forth surfaced it.

**Takeaway.** The architectural calls are yours. Agents sketch and
discuss alternatives. Treat design as written docs you iterate with
the agent. Resist the pull toward implementation.

---

## 4. Implementation

**Context management.** The most important thing for me in
implementation is picking the right context for the agent.
The agent can write code in any language, but it writes much better
code when it has the right examples to read. When I was bootstrapping
YoloFS, I cloned the Linux source and asked the agent to refer to
OverlayFS as the model for our stackable filesystem. That turned out
to be too much: OverlayFS is complex, and the agent was overwhelmed
by the details. I switched to WrapFS, a bare-minimum passthrough
stackable filesystem, and it worked very well as a starting point.
From there I let the agent read OverlayFS only when it needed
something specific. The same pattern showed up on the userspace side:
I pointed the agent at `try`, an existing tool that diffs and commits
changes to an OverlayFS, and that gave the implementation the right
shape from the start.

**Implement, test, review.** The agent implements against a plan,
runs the tests, diagnoses failures, and fixes bugs. Reliability
matters a lot here, so I have the agent write a lot of unit tests as
it goes. Each pass ends with code review. My `AGENTS.md` specifies
five parallel subagents, each examining the diff along one dimension:
bugs and correctness (catching logic errors, race conditions, and
unchecked inputs), code quality (flagging unnecessary allocations and
complex logic), doc consistency (verifying that `docs/` matches the
new behavior), missing tests (spotting untested code paths), and plan
adherence (confirming the diff follows the plan). The main agent triages the findings and revises. For
complex plans I run multiple rounds of review until it comes back
clean.

**Other notes.** All three stages (design, implementation, and
review) can be pipelined. At peak I had design happening on one task,
implementation running on another, and review on a third. The single
most powerful project rule, written into the very first `AGENTS.md`,
is *"No backwards compatibility is needed."* For bug fixes, the rule
is to write a failing test first, then fix it. This keeps the loop
honest about what is actually broken.

**Takeaway.** Pick the right context: minimal and concrete beats
large and complex. Build an agent loop that implements, tests, and
reviews.

---

## 5. Evaluation

The hardest part of evaluation is not running the benchmarks. It is
understanding the results.

**Visualization workflow.** With agents I can build very nice
frontends to make sense of the data, which used to be a lot of manual
work. My workflow has three layers. The system generates raw traces.
A separate Python script transforms the traces into JSON ready to
plot. An HTML frontend visualizes the results dynamically as new data
feeds in. Junxuan even built a differential benchmark view that
compares results across runs side by side.

**Agent's role.** The same Python layer can also aggregate results
into a form I can feed back to the agent, so it can reason over the
numbers rather than over raw traces. One gap I have not closed: unit
tests catch correctness issues easily, but they cannot catch
performance bugs. In practice performance bugs are still spotted by
humans. Once a human can reliably pinpoint a metric (the execution
time of a particular function, say), the agent is good at fixing and
optimizing.

**Takeaway.** Agents make it easy to build visualizations that help
you understand the results. Feed the same results back so the agent
can iterate and improve performance.

---

## 6. Paper writing

Agents can help with paper writing too, given the right context up
front.

**Writing guide.** A few days before I wrote a single word of the
paper, I built a writing guide from recent literature. I
downloaded the FAST, OSDI, and SOSP proceedings from the past three
years, asked the agent to filter out the filesystem papers, and then
ran a per-paper pass: each paper got its own Markdown summary of the
writing techniques it used (how the introduction is structured, how
motivation is built, how the design section is presented, how
evaluation is framed). A script enforced the control flow, the same
way it did for the misuse study. Once all the per-paper summaries
were on disk, I asked a second agent to read across them and
synthesize a single `writing-guide.md` organized by paper section. The guide
pinpoints very specific techniques: quantified
hooks in introductions, taxonomy-then-gap argumentation in
motivation, named abstractions as shorthand in design, layered
micro→macro→real-world evaluation funnels. Each one is grounded in
concrete examples from the data set.

**Reference context.** Two things matter for the reference context.
The design docs in `docs/` give the agent the high-level idea of the
system, so it does not have to reverse-engineer the architecture from
code. And the narrative arc still has to come from me: what the paper
is *about*, what each section is doing, why the order is what it is.
I deliberately do *not* feed raw kernel and userspace code into the
drafting agent. With the source in context, the agent latches onto
low-level details and writes a system manual instead of a paper.

**Drafting loop.** I start with a very rough draft of what I want to
write, full of grammar errors and half-formed sentences. Sometimes I
point the agent to where to find the information. Sometimes it
figures out the source itself. The initial agent output is never at
the bar for the final paper, so I iterate on each sentence until it
lands. Once a paragraph is done, I ask the agent whether the flow
makes sense, and it uses the writing guide to help. At the end, I run
subagents in parallel to catch grammar mistakes and term
inconsistencies.

**Example.** When I was writing about the challenges of existing
permission models, I gave the agent content on the unix permission
model, security mechanisms, and mount-based solutions. The agent came
up with the term *monotonic permission* on its own, and I kept it.
Agents are surprisingly good at finding the precise term for what you
want. *Corrective and preventive control* is another phrase that came
from the agent.

**Takeaway.** Build a writing guide before drafting. Feed the agent
design docs and the narrative arc, not raw code.

---

## 7. General takeaways

- **Everything is a file.** Files are the universal interface agents
  already know: read, write, grep, list, diff. Structuring research
  artifacts (literature notes, corpus, plans, paper sections, working
  rules, generated stats) as files lets the agent operate without
  bespoke databases, proprietary UIs, or APIs to learn.

- **Make agent output human-verifiable.** Agents fabricate plausibly.
  Each load-bearing claim should point back to something a human can
  open and check: a generated stat file, a YAML field in a report, a
  `.bib` entry, a regenerated CSV.

- **Layered `AGENTS.md` files.** One per domain (code, writing, study).
  Each under 100 lines. Each rule corresponds to a past failure.

- **Subagents for fan-out.** Multi-dimensional review on a single diff;
  exploration agents that summarize without polluting context.

- **Static control flow over agents.** Scripts orchestrate per-unit
  agent calls and aggregate the results deterministically. The agent
  does not orchestrate itself.

- **Pick the right context.** The agent writes much better code,
  prose, or analysis when given the right concrete examples to read.
  Too much context is as bad as too little: a minimal, well-chosen
  reference beats a large, complex one.
