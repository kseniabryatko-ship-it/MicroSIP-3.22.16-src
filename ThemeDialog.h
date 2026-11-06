/*
 * Copyright (C) 2011-2026 MicroSIP (http://www.microsip.org)
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 2 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software
 * Foundation, Inc., 59 Temple Place, Suite 330, Boston, MA  02111-1307  USA
 */

#pragma once

class CThemeDialog : public CDialog
{
    // Construction
public:
    CThemeDialog(UINT nIDTemplate, CWnd* pParent = NULL);   // standard constructor

    // Overrides
    // ClassWizard generated virtual function overrides
    //{{AFX_VIRTUAL(CThemeDialog)
protected:
    //}}AFX_VIRTUAL

protected:
    bool m_bAllowDark = false;
    //{{AFX_MSG(CThemeDialog)
    virtual BOOL OnInitDialog();
    afx_msg HBRUSH OnCtlColor(CDC* pDC, CWnd* pWnd, UINT nCtlColor);
    afx_msg void OnSettingChange(UINT uFlags, LPCTSTR lpszSection);
    //}}AFX_MSG
    void ApplyTheme();
    BYTE lastDark;
    DECLARE_MESSAGE_MAP()

private:
};
