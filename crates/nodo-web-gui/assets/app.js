// The Nodo's admin pages: the little that HTML and htmx do not do by themselves. Every page works
// without this file. It is served from this origin, as the content security policy requires.

// A question that htmx puts over the page takes the keyboard: the focus goes to its default
// answer (Cancel, for anything that cannot be taken back), not to whatever is under the dialog.
document.addEventListener('htmx:after:swap', function () {
  var answer = document.querySelector('#dialog [autofocus]');
  if (answer) answer.focus();
});

// Cancel takes the dialog away and leaves the page as it is, with whatever is typed into its
// form. Without this file Cancel is a link that loads the page again.
function cancel(event) {
  var dialog = document.getElementById('dialog');
  if (!dialog || !dialog.firstChild) return false;
  if (event) event.preventDefault();
  dialog.replaceChildren();
  return true;
}

document.addEventListener('click', function (event) {
  if (event.target.closest && event.target.closest('#dialog a.btn')) cancel(event);
});

// Escape answers a question with Cancel.
document.addEventListener('keydown', function (event) {
  if (event.key === 'Escape') cancel(event);
});

// A value with a Copy button goes to the clipboard. The value is on the page either way.
document.addEventListener('click', function (event) {
  var button = event.target.closest && event.target.closest('button.copy');
  if (!button || !navigator.clipboard) return;
  navigator.clipboard.writeText(button.dataset.copy).then(function () {
    button.textContent = 'Copied';
    setTimeout(function () { button.textContent = 'Copy'; }, 1500);
  });
});

// The list of installed apps: Tick all, and how many are ticked, beside the buttons that act on
// them. Without this file the boxes are ticked one by one and the bar has no count.
function tickedCount(form) {
  var boxes = document.querySelectorAll('input[type=checkbox][form="' + form.id + '"]');
  var ticked = 0;
  boxes.forEach(function (box) { if (box.checked) ticked += 1; });
  var count = form.querySelector('.ticked-count');
  if (count) count.textContent = ticked + ' ticked';
  var all = document.querySelector('input.tick-all');
  if (all) {
    all.checked = boxes.length > 0 && ticked === boxes.length;
    all.indeterminate = ticked > 0 && ticked < boxes.length;
  }
}

function setUpTicks() {
  var form = document.getElementById('ticked');
  if (!form) return;
  var all = document.querySelector('input.tick-all');
  if (all) all.hidden = false;
  tickedCount(form);
}

document.addEventListener('change', function (event) {
  var form = document.getElementById('ticked');
  if (!form || !event.target.matches) return;
  if (event.target.matches('input.tick-all')) {
    document.querySelectorAll('input[type=checkbox][form="ticked"]').forEach(function (box) { box.checked = event.target.checked; });
  }
  if (event.target.matches('input.tick-all, input[type=checkbox][form="ticked"]')) tickedCount(form);
});

document.addEventListener('DOMContentLoaded', setUpTicks);
document.addEventListener('htmx:after:swap', setUpTicks);
