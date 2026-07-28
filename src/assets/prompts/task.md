Capture a task for this workspace **without changing what you are doing**.

Run:

```sh
ws -task add "<the task, in one line>"
```

`-task add` defaults to the workspace you are already in, so do not pass a name.

Then carry on with whatever you were working on. Do not start the captured task,
do not plan it, and do not ask whether to switch to it — the whole point of this
command is that a thought can be written down without derailing the current one.
If the task needs detail that will not fit on one line, capture the one-line
version now and write the detail into your notebook
(`.ws/notebook/notebook.<actor>.md`).

Acknowledge in a single short sentence naming what you captured, then resume.
