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
