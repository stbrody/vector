require 'word_wrap/core_ext'

class String
  # Comments out a block of text
  def commentify
    "# " + self.gsub("\n", "\n# ")
  end

  # Downcases the first letter, even if it has markdown syntax
  def continuize
    i = 0
    
    loop do
      if i > self.length
        break
      end

      if self[i] != "["
        self[i] = self[i].downcase
        break
      end

      i = i+1
    end

    self
  end

  def editorify(width = 80)
    self.
      remove_markdown_links.
      wrap(width)
  end

  def html_escape
    ERB::Util.html_escape(self)
  end

  def remove_markdown_links
    self.
      gsub(/\[([^\]]+)\]\(([^) ]+)\)/, '\1').
      gsub(/\[([^\]]+)\]\[([^) ]+)\]/, '\1')
  end

  def replace(match, sub)
    gsub match, match => sub
  end

  def replace!(match, sub)
    gsub! match, match => sub
  end

  def table_escape
    gsub("|", '\|')
  end
end
