// The Nodo's admin pages: the little that HTML and htmx do not do by themselves. Every page works
// without this file. It is served from this origin, as the content security policy requires.

// A question that htmx puts over the page takes the keyboard: the focus goes to its default
// answer (Cancel, for anything that cannot be taken back), not to whatever is under the dialog.
document.addEventListener('htmx:after:swap', function () {
  var answer = document.querySelector('#dialog [autofocus]');
  if (answer) answer.focus();
});

// Escape answers a question with Cancel.
document.addEventListener('keydown', function (event) {
  if (event.key !== 'Escape') return;
  var cancel = document.querySelector('#dialog a.btn');
  if (cancel) cancel.click();
});
