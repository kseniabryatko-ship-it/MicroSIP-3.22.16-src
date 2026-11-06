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

#include "ThemeDialog.h"
#include "resource.h"
#include "global.h"

class ExportDlg :
	public CThemeDialog
{
public:
	ExportDlg(CWnd* pParent = NULL);	// standard constructor
	~ExportDlg();
	enum { IDD = IDD_EXPORT };

protected:

	virtual BOOL OnInitDialog();
	afx_msg void OnDestroy();
	virtual void PostNcDestroy();
	DECLARE_MESSAGE_MAP()
private:
	void InitCheckbox(CString section, int nID);
public:
	afx_msg int OnCreate(LPCREATESTRUCT lpCreateStruct);
	afx_msg void OnClose();
	afx_msg void OnBnClickedCancel();
	afx_msg void OnBnClickedOk();
	afx_msg void OnDeltaposSpinModify(NMHDR *pNMHDR, LRESULT *pResult);
	afx_msg int OnVKeyToItem(UINT, CListBox*, UINT);
	afx_msg void OnDeltaposSpinOrder(NMHDR *pNMHDR, LRESULT *pResult);
};

