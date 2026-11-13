return {
  api_version = 1,
  settings = {
    {
      name = "public_url",
      type = "url",
      label = "Public address",
      help = "The address the invite and personal links use, such as https://kringle.example.org. "
        .. "Empty, each link uses the address the browser used (--public-url).",
      validate = function(url)
        return nodo.matches(url, "^https?://[A-Za-z0-9.\\[\\]:-]+$"),
          "must be http:// or https:// with no path or trailing slash"
      end,
    },
    {
      name = "keep_days",
      type = "int",
      label = "Delete groups after (days)",
      help = "Groups are deleted this many days after they were started (--keep-days). Groups "
        .. "nobody but the host joined go after a week.",
      default = 120,
      min = 1,
      max = 3650,
    },
  },
}
