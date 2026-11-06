#pragma once

#include <afxcmn.h>

class CLevelsSliderCtrl : public CSliderCtrl
{
public:
	CLevelsSliderCtrl(){IsActive = false;};
	bool IsActive = false;
	bool IsDark = false;

protected:
	DECLARE_MESSAGE_MAP()
	afx_msg void OnCustomDraw(NMHDR *pNotifyStruct, LRESULT *result);
};
